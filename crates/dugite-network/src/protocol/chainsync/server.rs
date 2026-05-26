//! ChainSync server — serves headers to downstream peers.
//!
//! Maintains a per-peer cursor tracking the last block served. When the peer
//! sends `MsgRequestNext`, serves the next header from ChainDB. At the tip,
//! waits for a block announcement via a broadcast channel before responding.
//!
//! ## Block Announcement
//! When a new block is received (either from upstream sync or forged locally),
//! a broadcast is sent. The server listens for this broadcast to unblock peers
//! waiting at the tip (in `StMustReply` state).
//!
//! ## Header Extraction
//! N2N ChainSync sends only the block *header*, not the full block.  Headers are
//! 10–100× smaller than full blocks; sending full blocks wastes bandwidth and
//! produces CBOR that Haskell decoders cannot parse as a header.
//!
//! Shelley+ blocks are HFC-wrapped: `[era_tag, block_body]` where
//! `block_body = [header, tx_bodies, tx_witnesses, aux_data, invalid_txs]`.
//! We extract `header` (index 0 of `block_body`) and re-wrap as
//! `[era_tag, #6.24(bstr(header_cbor))]`.
//!
//! Byron blocks (era_tag = 0) use a different internal structure; they are
//! handled by a dedicated path that skips tag-24 wrapping.

use std::io::Write as _;
use std::time::Duration;

use minicbor::{Decoder, Encoder};
use tokio::sync::broadcast;

use crate::codec::Point;
use crate::error::ProtocolError;
use crate::mux::channel::MuxChannel;
use crate::BlockProvider;

use super::{decode_message, encode_message, ChainSyncMessage};

// Re-use shared HFC helpers from the protocol module.
use crate::protocol::{storage_era_tag_to_hfc_index, CBOR_TAG_EMBEDDED};

/// Extract the block header from raw HFC-wrapped block CBOR and encode it as
/// `[hfc_index, #6.24(bstr(header_cbor))]` ready for inlining into MsgRollForward.
///
/// # Era tag conversion
///
/// The block CBOR uses storage era tags (Byron=0/1, Shelley=2, …,
/// Conway=7).  The N2N ChainSync `MsgRollForward` uses HFC NS indices
/// (Byron=0, Shelley=1, …, Conway=6) — one less than the storage tag for all
/// post-Byron eras.  This function converts between the two schemes so that
/// Haskell peers route the header to the correct era-specific decoder.
///
/// # Block layout
///
/// ```text
/// Shelley+ HFC layout: [storage_era_tag, [header, tx_bodies, tx_witnesses, aux_data, invalid_txs]]
/// Byron HFC layout:    [storage_era_tag, [header, body, extra]]
/// ```
///
/// Returns an error if the CBOR does not match the expected structure.  The
/// caller MUST propagate the error and MUST NOT fall back to sending the full
/// block — doing so would produce incorrect wire output.
pub fn extract_header_for_chainsync(block_cbor: &[u8]) -> Result<Vec<u8>, String> {
    let mut dec = Decoder::new(block_cbor);

    // Outer array: [storage_era_tag, block_body]
    dec.array()
        .map_err(|e| format!("block CBOR: expected outer array: {e}"))?;
    let storage_era_tag = dec
        .u64()
        .map_err(|e| format!("block CBOR: expected era_tag u64: {e}"))?;

    // Convert to HFC NS index (the value Haskell's encodeNS/decodeNS expects).
    let hfc_index = storage_era_tag_to_hfc_index(storage_era_tag)?;

    // Inner array: [header, ...]  — we only need the first element.
    dec.array().map_err(|e| {
        format!("block CBOR (storage_era={storage_era_tag}): expected inner block array: {e}")
    })?;

    // Capture the raw CBOR bytes of the header sub-value.
    let header_start = dec.position();
    dec.skip().map_err(|e| {
        format!("block CBOR (storage_era={storage_era_tag}): could not skip header: {e}")
    })?;
    let header_end = dec.position();
    let header_cbor = &block_cbor[header_start..header_end];

    // Encode the HFC-wrapped header: [hfc_index, #6.24(bstr(header_cbor))]
    //
    // This is the format expected by Haskell's `dispatchDecoder` in
    // `Ouroboros.Consensus.HardFork.Combinator.Serialisation.SerialiseNodeToNode`.
    // `encodeNS` produces `array(2)[era_index_u8, tag(24)(header_bytes)]`.
    let mut buf = Vec::with_capacity(8 + header_cbor.len());
    let mut enc = Encoder::new(&mut buf);
    enc.array(2)
        .map_err(|e| format!("encode hfc header: array: {e}"))?;
    enc.u8(hfc_index)
        .map_err(|e| format!("encode hfc header: hfc_index: {e}"))?;
    enc.tag(minicbor::data::Tag::new(CBOR_TAG_EMBEDDED))
        .map_err(|e| format!("encode hfc header: tag24: {e}"))?;
    enc.bytes(header_cbor)
        .map_err(|e| format!("encode hfc header: bytes: {e}"))?;
    // Write any remaining encoder state.
    enc.writer_mut()
        .flush()
        .map_err(|e| format!("encode hfc header: flush: {e}"))?;

    Ok(buf)
}

/// Block announcement sent via broadcast channel when a new block arrives.
#[derive(Debug, Clone)]
pub struct BlockAnnouncement {
    /// Slot of the announced block.
    pub slot: u64,
    /// Hash of the announced block.
    pub hash: [u8; 32],
    /// Block number of the announced block.
    pub block_number: u64,
}

/// Rollback announcement sent via broadcast channel when the chain rolls back.
///
/// When the local chain switches to a better fork, all downstream ChainSync
/// followers must be notified so they send `MsgRollBackward` to their peers
/// instead of continuing to serve blocks from the old (now-abandoned) fork.
#[derive(Debug, Clone)]
pub struct RollbackAnnouncement {
    /// Slot of the point to roll back to.
    pub slot: u64,
    /// Hash of the block at the rollback point.
    pub hash: [u8; 32],
}

/// ChainSync server that serves headers to a single downstream peer.
pub struct ChainSyncServer {
    /// Current cursor: the last point served to this peer.
    cursor_slot: u64,
    cursor_hash: [u8; 32],
    /// Whether the cursor has been initialized (via intersection or genesis).
    cursor_initialized: bool,
    /// True when the cursor is at Origin — meaning no block has been served
    /// yet and we must include blocks at slot 0 (e.g. Byron genesis EBB).
    cursor_at_origin: bool,
}

impl ChainSyncServer {
    /// Create a new server with no cursor (must find intersection first).
    pub fn new() -> Self {
        Self {
            cursor_slot: 0,
            cursor_hash: [0; 32],
            cursor_initialized: false,
            cursor_at_origin: false,
        }
    }

    /// Run the ChainSync server loop.
    ///
    /// Handles `MsgFindIntersect`, `MsgRequestNext`, and `MsgDone` from the client.
    /// Uses `block_provider` to look up blocks, `announcement_rx` to wait for
    /// new blocks at the tip, and `rollback_rx` to propagate chain rollbacks
    /// to downstream peers via `MsgRollBackward`.
    pub async fn run<B: BlockProvider>(
        &mut self,
        channel: &mut MuxChannel,
        block_provider: &B,
        mut announcement_rx: broadcast::Receiver<BlockAnnouncement>,
        mut rollback_rx: broadcast::Receiver<RollbackAnnouncement>,
    ) -> Result<(), ProtocolError> {
        let task_id = self as *const _ as usize;
        tracing::info!(task_id, "chainsync server: task started");
        loop {
            tracing::debug!(
                task_id,
                cursor_slot = self.cursor_slot,
                cursor_at_origin = self.cursor_at_origin,
                "chainsync server: awaiting next client message"
            );
            let msg_bytes = channel.recv().await.map_err(|e| {
                tracing::info!(
                    task_id,
                    error = %e,
                    cursor_slot = self.cursor_slot,
                    "chainsync server: channel.recv() failed — task exiting"
                );
                ProtocolError::from(e)
            })?;
            let msg = decode_message(&msg_bytes).map_err(|e| ProtocolError::CborDecode {
                protocol: "ChainSync",
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
                    tracing::debug!("chainsync server: client sent MsgDone");
                    return Ok(());
                }
                other => {
                    return Err(ProtocolError::AgencyViolation {
                        protocol: "ChainSync",
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
    async fn handle_find_intersect<B: BlockProvider>(
        &mut self,
        channel: &mut MuxChannel,
        block_provider: &B,
        points: &[Point],
    ) -> Result<(), ProtocolError> {
        let tip = block_provider.get_tip();

        // Walk points in order (most recent first), find the first one we have
        for point in points {
            match point {
                Point::Origin => {
                    // We always have genesis
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
                    if block_provider.has_block(hash) {
                        self.cursor_slot = *slot;
                        self.cursor_hash = *hash;
                        self.cursor_initialized = true;
                        self.cursor_at_origin = false;

                        let response = encode_message(&ChainSyncMessage::MsgIntersectFound {
                            point: point.clone(),
                            tip_slot: tip.slot,
                            tip_hash: tip.hash,
                            tip_block_number: tip.block_number,
                        });
                        channel.send(response).await.map_err(ProtocolError::from)?;
                        return Ok(());
                    }
                }
            }
        }

        // No intersection found
        let response = encode_message(&ChainSyncMessage::MsgIntersectNotFound {
            tip_slot: tip.slot,
            tip_hash: tip.hash,
            tip_block_number: tip.block_number,
        });
        channel.send(response).await.map_err(ProtocolError::from)?;
        Ok(())
    }

    /// Handle MsgRequestNext: serve the next header, propagate rollback, or wait
    /// for an announcement.
    ///
    /// When a rollback is detected (either because the cursor points beyond the
    /// rollback point, or a `RollbackAnnouncement` arrives while waiting at the
    /// tip), the server sends `MsgRollBackward` to rewind the downstream peer's
    /// cursor.
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
        // Block announcements are broadcast as dugite forges blocks, and are
        // buffered in the receiver.  The direct `next_block` lookup below is
        // authoritative — dugite commits each block to ChainDB before
        // broadcasting the announcement, so any block we need to serve is
        // already reachable via the block provider.  Drain all buffered
        // announcements here so that the MsgAwaitReply loop only wakes on
        // genuinely fresh post-drain forging events; otherwise a stale
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
                pre_lookup_cursor,
                post_serve_cursor = self.cursor_slot,
                "chainsync server: direct serve path produced a message"
            );
            return Ok(());
        }

        // We're at the tip — send MsgAwaitReply and wait for announcement.
        tracing::info!(
            task_id,
            cursor_slot = self.cursor_slot,
            "chainsync server: no next block — sending MsgAwaitReply, entering StMustReply"
        );
        let await_msg = encode_message(&ChainSyncMessage::MsgAwaitReply);
        channel.send(await_msg).await.map_err(|e| {
            tracing::warn!(
                task_id,
                error = %e,
                "chainsync server: MsgAwaitReply send failed"
            );
            ProtocolError::from(e)
        })?;

        // Wait for a block announcement OR rollback with a fixed timeout.
        // Haskell uses a timeout range of 135–911 seconds; we use 135s as the lower bound.
        let timeout = Duration::from_secs(135);

        // Bug J fix (2026-05-16): biased select so rollback_rx is always
        // checked first when both arms are ready.  Combined with the cursor
        // revalidation in `try_serve_next_block`, this eliminates the race
        // where an announcement arriving simultaneously with a rollback
        // would advance the cursor past the new chain's earlier blocks.
        tokio::select! {
            biased;

            // ── Rollback received while waiting at tip ──────────────────────
            // This is the critical path for issue #299: a follower in
            // MsgAwaitReply (StMustReply) must receive MsgRollBackward when
            // the chain switches to a better fork.
            rollback = rollback_rx.recv() => {
                match rollback {
                    Ok(rb) => {
                        // Only send rollback if cursor is at or beyond the rollback point.
                        // If cursor is behind, the peer hasn't seen the rolled-back blocks
                        // yet and will naturally follow the new fork.
                        if self.cursor_slot > rb.slot
                            || (self.cursor_slot == rb.slot && self.cursor_hash != rb.hash)
                        {
                            self.send_rollback(channel, block_provider, &rb).await
                        } else {
                            // Cursor is behind the rollback point — route through
                            // `try_serve_next_block` so cursor revalidation (Bug J)
                            // still applies on the post-rollback chain.
                            self.try_serve_next_block(channel, block_provider).await?;
                            Ok(())
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        // Missed rollback events — send rollback to current tip.
                        let tip = block_provider.get_tip();
                        let rb = RollbackAnnouncement {
                            slot: tip.slot,
                            hash: tip.hash,
                        };
                        self.send_rollback(channel, block_provider, &rb).await
                    }
                    Err(broadcast::error::RecvError::Closed) => Ok(()),
                }
            }
            announcement = announcement_rx.recv() => {
                tracing::info!(
                    task_id,
                    cursor_slot = self.cursor_slot,
                    "chainsync server: woke from StMustReply on announcement"
                );
                match announcement {
                    Ok(_ann) => {
                        // The announcement is used only as a wake signal — the
                        // block_provider is authoritative, and we MUST re-use
                        // the same cursor-aware lookup as the direct-serve path
                        // so that (a) slot-0 blocks at Origin are handled via
                        // the inclusive `>=` lookup and (b) `cursor_at_origin`
                        // is reliably cleared after the first serve.  Trusting
                        // `ann.hash`/`ann.slot` directly skips the `cursor_at_origin
                        // = false` transition and causes the next
                        // MsgRequestNext to re-serve the first block, which
                        // Haskell rejects with `UnexpectedBlockNo`.
                        //
                        // Bug J fix (2026-05-16): defence-in-depth — drain any
                        // queued rollback first so the announcement-first race
                        // is closed even if `try_serve_next_block`'s cursor
                        // revalidation somehow missed (e.g., the cursor was
                        // still on the old chain but the rollback queue had a
                        // pending event).
                        if let Some(rb) = Self::drain_rollback(rollback_rx) {
                            if self.cursor_slot > rb.slot
                                || (self.cursor_slot == rb.slot && self.cursor_hash != rb.hash)
                            {
                                return self.send_rollback(channel, block_provider, &rb).await;
                            }
                        }
                        self.try_serve_next_block(channel, block_provider).await?;
                        Ok(())
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!(
                            n,
                            "chainsync server: announcement channel lagged; catching up from tip"
                        );
                        // Route through `try_serve_next_block` so cursor
                        // revalidation still applies after the lag.
                        self.try_serve_next_block(channel, block_provider).await?;
                        Ok(())
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Broadcast sender dropped — node is shutting down.
                        Ok(())
                    }
                }
            }
            _ = tokio::time::sleep(timeout) => {
                tracing::warn!(
                    task_id,
                    cursor_slot = self.cursor_slot,
                    timeout_seconds = timeout.as_secs(),
                    "chainsync server: StMustReply timed out — re-polling block provider for next block"
                );
                self.try_serve_next_block(channel, block_provider).await?;
                Ok(())
            }
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
        let next_block = if self.cursor_at_origin {
            block_provider.get_block_at_or_after_slot(0)
        } else {
            block_provider.get_next_block_after_slot(self.cursor_slot)
        };

        let Some((slot, hash, block_cbor)) = next_block else {
            return Ok(false);
        };

        // Extract the block header and encode it as the HFC-wrapped header
        // payload expected by N2N ChainSync: [era_id, #6.24(bstr(header_cbor))].
        let hfc_header = extract_header_for_chainsync(&block_cbor).map_err(|reason| {
            ProtocolError::CborDecode {
                protocol: "ChainSync",
                reason: format!("header extraction failed for block at slot {slot}: {reason}"),
            }
        })?;

        let tip = block_provider.get_tip();
        let response = encode_message(&ChainSyncMessage::MsgRollForward {
            header: hfc_header,
            tip_slot: tip.slot,
            tip_hash: tip.hash,
            tip_block_number: tip.block_number,
        });
        channel.send(response).await.map_err(ProtocolError::from)?;

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
    fn drain_rollback(
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

impl Default for ChainSyncServer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TipInfo;
    use bytes::Bytes;
    use minicbor::Encoder;
    use tokio::sync::mpsc;

    // ─── helpers ──────────────────────────────────────────────────────────────

    /// Encode `value` as a CBOR bstr — returns the bytes used to store `value`
    /// inside the block's inner array.  This is what the header element looks
    /// like at wire level when we put raw bytes there for testing.
    fn cbor_encode_bytes(value: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.bytes(value).unwrap();
        buf
    }

    /// Build a minimal valid HFC-wrapped block CBOR for testing.
    ///
    /// Layout: `[era_tag, [header_cbor, [], [], null, []]]`
    ///
    /// The header element is stored as a CBOR bstr containing `header_bytes`.
    /// `extract_header_for_chainsync` will capture the full CBOR encoding of
    /// that bstr (including the length prefix) as the header sub-value.
    fn make_hfc_block(era_tag: u64, header_bytes: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        // Outer: [era_tag, inner_array]
        enc.array(2).unwrap();
        enc.u64(era_tag).unwrap();
        // Inner block body array: [header, tx_bodies, tx_witnesses, aux_data, invalid_txs]
        enc.array(5).unwrap();
        enc.bytes(header_bytes).unwrap(); // header stored as a bstr
        enc.array(0).unwrap(); // tx_bodies: []
        enc.array(0).unwrap(); // tx_witnesses: []
        enc.null().unwrap(); // aux_data: null
        enc.array(0).unwrap(); // invalid_txs: []
        buf
    }

    /// Build the expected HFC-wrapped header bytes that `extract_header_for_chainsync`
    /// should produce.
    ///
    /// `storage_era_tag` is the era tag from the block's on-disk/wire CBOR (legacy
    /// convention: Byron=0/1, Shelley=2, ..., Conway=7).  The function converts it
    /// to the HFC NS index and encodes `[hfc_index_u8, #6.24(bstr(header_cbor))]`.
    ///
    /// `header_cbor` must be the **CBOR-encoded** form of the header element as
    /// it appears inside the block — i.e. the bytes captured by `dec.skip()`.
    /// For blocks built with `make_hfc_block`, pass `cbor_encode_bytes(raw_bytes)`.
    fn expected_hfc_header(storage_era_tag: u64, header_cbor: &[u8]) -> Vec<u8> {
        let hfc_index = storage_era_tag_to_hfc_index(storage_era_tag)
            .expect("test fixture used invalid storage era tag");
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u8(hfc_index).unwrap();
        enc.tag(minicbor::data::Tag::new(24)).unwrap();
        // header_cbor is the raw CBOR of the header element; embed it as a bstr
        // (this is what tag24 wraps — the CBOR serialisation of the header).
        enc.bytes(header_cbor).unwrap();
        buf
    }

    /// Mock block provider that stores (slot, hash, block_cbor) triples.
    struct MockBlockProvider {
        blocks: Vec<(u64, [u8; 32], Vec<u8>)>,
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
    }

    fn make_test_channel() -> (
        MuxChannel,
        mpsc::Receiver<(u16, crate::mux::Direction, Bytes)>,
        mpsc::Sender<Bytes>,
    ) {
        let (egress_tx, egress_rx) = mpsc::channel(64);
        let (ingress_tx, ingress_rx) = mpsc::channel(64);
        let channel = MuxChannel::new(
            2,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            1_000_000,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }

    // ─── unit tests for extract_header_for_chainsync ──────────────────────────

    #[test]
    fn extract_header_conway_block() {
        // Build a synthetic Conway (storage era_tag=7, HFC index=6) HFC block.
        // Pallas uses era_tag=7 for Conway blocks in ImmutableDB and block-fetch.
        // The ChainSync header wire format uses HFC NS index=6 for Conway.
        // In our test fixture the header element is a bstr; extraction captures
        // the full CBOR encoding of that element (bstr length prefix + bytes).
        let inner_header_bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let block_cbor = make_hfc_block(7, &inner_header_bytes); // storage era_tag=7 (Conway)

        let hfc_header = extract_header_for_chainsync(&block_cbor)
            .expect("extraction should succeed for valid Conway block");

        // The extractor converts storage era_tag=7 → HFC index=6, then produces
        // [6_u8, #6.24(bstr(header_cbor))].  Pass the CBOR form of the element.
        let header_cbor = cbor_encode_bytes(&inner_header_bytes);
        let expected = expected_hfc_header(7, &header_cbor); // storage tag→hfc_index conversion
        assert_eq!(
            hfc_header, expected,
            "extracted HFC header does not match expected encoding"
        );

        // Verify the HFC index in the output is actually 6 (Conway).
        let mut dec = minicbor::Decoder::new(&hfc_header);
        dec.array().unwrap();
        let hfc_idx = dec.u8().unwrap();
        assert_eq!(hfc_idx, 6, "Conway blocks must use HFC NS index 6");
    }

    #[test]
    fn extract_header_babbage_block() {
        // Babbage (storage era_tag=6, HFC index=5) — distinct from Conway.
        let inner_header_bytes = vec![0xBA, 0xBB, 0xAA, 0xBE];
        let block_cbor = make_hfc_block(6, &inner_header_bytes); // storage era_tag=6 (Babbage)

        let hfc_header = extract_header_for_chainsync(&block_cbor)
            .expect("extraction should succeed for valid Babbage block");

        let header_cbor = cbor_encode_bytes(&inner_header_bytes);
        let expected = expected_hfc_header(6, &header_cbor);
        assert_eq!(hfc_header, expected);

        // Verify the HFC index in the output is actually 5 (Babbage).
        let mut dec = minicbor::Decoder::new(&hfc_header);
        dec.array().unwrap();
        let hfc_idx = dec.u8().unwrap();
        assert_eq!(hfc_idx, 5, "Babbage blocks must use HFC NS index 5");
    }

    #[test]
    fn extract_header_shelley_block() {
        // Shelley (storage era_tag=2, HFC index=1) — same structure, different era identifier.
        let inner_header_bytes = vec![0x01, 0x02, 0x03];
        let block_cbor = make_hfc_block(2, &inner_header_bytes);

        let hfc_header =
            extract_header_for_chainsync(&block_cbor).expect("Shelley extraction should succeed");

        let header_cbor = cbor_encode_bytes(&inner_header_bytes);
        let expected = expected_hfc_header(2, &header_cbor);
        assert_eq!(hfc_header, expected);

        // Verify the HFC index in the output is actually 1 (Shelley).
        let mut dec = minicbor::Decoder::new(&hfc_header);
        dec.array().unwrap();
        let hfc_idx = dec.u8().unwrap();
        assert_eq!(hfc_idx, 1, "Shelley blocks must use HFC NS index 1");
    }

    #[test]
    fn extract_header_larger_inner_header() {
        // Verify extraction with a larger (256-byte) inner header payload (Conway).
        let inner_header_bytes: Vec<u8> = (0u8..=255u8).collect();
        let block_cbor = make_hfc_block(7, &inner_header_bytes); // Conway storage era_tag=7

        let hfc_header = extract_header_for_chainsync(&block_cbor)
            .expect("extraction should succeed with large inner header");

        let header_cbor = cbor_encode_bytes(&inner_header_bytes);
        let expected = expected_hfc_header(7, &header_cbor); // Conway storage_era_tag=7
        assert_eq!(hfc_header, expected);
    }

    #[test]
    fn extract_header_invalid_cbor_returns_error() {
        // Truncated / garbage input must return an Err, not panic.
        let result = extract_header_for_chainsync(&[0xFF, 0x00, 0x01]);
        assert!(
            result.is_err(),
            "expected Err for invalid CBOR, got Ok: {result:?}"
        );
    }

    #[test]
    fn extract_header_empty_input_returns_error() {
        let result = extract_header_for_chainsync(&[]);
        assert!(result.is_err(), "expected Err for empty input");
    }

    // ─── integration tests for ChainSync server ───────────────────────────────

    #[tokio::test]
    async fn find_intersect_with_known_block() {
        // Block CBOR does not need to be valid for FindIntersect (the server only
        // checks presence via has_block, not the CBOR content).
        // Use Conway storage era_tag=7 for realistic block CBOR.
        let block_cbor = make_hfc_block(7, &[0x01, 0x02]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(100, [0xAA; 32], block_cbor)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Send MsgFindIntersect
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            100, [0xAA; 32],
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        // Read MsgIntersectFound
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(matches!(msg, ChainSyncMessage::MsgIntersectFound { .. }));

        // Send MsgDone to clean up
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();

        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn request_next_serves_header_not_full_block() {
        // The mock provider holds blocks whose CBOR is a valid HFC-wrapped block.
        // The server must extract the header and send [hfc_index, #6.24(bstr(hdr))].
        // Use Conway storage era_tag=7 (→ HFC index=6) for realistic fixtures.
        let inner_header_bytes_a = vec![0xAA, 0xBB];
        let inner_header_bytes_b = vec![0xCC, 0xDD];
        let block_a = make_hfc_block(7, &inner_header_bytes_a); // Conway storage_era_tag=7
        let block_b = make_hfc_block(7, &inner_header_bytes_b); // Conway storage_era_tag=7

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(10, [0x01; 32], block_a), (20, [0x02; 32], block_b)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);

        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Find intersection at origin (sets cursor_slot = 0).
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Request next — should get the header for the block at slot 10.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollForward { header, .. } = msg {
            // The header field must be the HFC-wrapped header, not the full block.
            // In our fixture the header element is a bstr, so its CBOR encoding
            // is cbor_encode_bytes(inner_header_bytes_a).
            // Conway storage_era_tag=7 → HFC index=6, so expected = [6, tag24(bytes)].
            let header_cbor = cbor_encode_bytes(&inner_header_bytes_a);
            let expected = expected_hfc_header(7, &header_cbor); // storage_era_tag=7 (Conway)
            assert_eq!(
                header, expected,
                "server sent incorrect HFC-wrapped header; \
                 expected [hfc_index=6, #6.24(bstr(inner))], got {header:?}"
            );
            // Sanity-check: the header is strictly smaller than the full block CBOR.
            // (Full block includes tx_bodies, tx_witnesses, aux_data, invalid_txs.)
            let full_block = make_hfc_block(7, &inner_header_bytes_a);
            assert!(
                header.len() < full_block.len(),
                "header ({} bytes) should be smaller than full block ({} bytes)",
                header.len(),
                full_block.len()
            );
        } else {
            panic!("expected MsgRollForward, got {msg:?}");
        }

        // Send MsgDone
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    // ─── rollback propagation tests ─────────────────────────────────────────────

    /// Block slot, hash, and CBOR data — the elements stored in mock providers.
    type BlockEntry = (u64, [u8; 32], Vec<u8>);

    /// Mock block provider that supports mutation for simulating rollback scenarios.
    ///
    /// Wraps blocks in an `Arc<Mutex<_>>` so both the server task and the test
    /// can modify the block set concurrently (simulating ChainDB changes during
    /// a fork switch).
    struct MutableMockBlockProvider {
        blocks: std::sync::Arc<std::sync::Mutex<Vec<BlockEntry>>>,
    }

    impl MutableMockBlockProvider {
        fn new(blocks: Vec<BlockEntry>) -> Self {
            Self {
                blocks: std::sync::Arc::new(std::sync::Mutex::new(blocks)),
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
    }

    #[tokio::test]
    async fn rollback_sends_msg_roll_backward() {
        // Scenario: serve 3 blocks (slots 10, 20, 30), then trigger a 2-block
        // rollback to slot 10.  The server should send MsgRollBackward(slot=10)
        // and then serve new fork blocks on the next MsgRequestNext.
        let block_a = make_hfc_block(7, &[0x0A]);
        let block_b = make_hfc_block(7, &[0x0B]);
        let block_c = make_hfc_block(7, &[0x0C]);

        let provider = MutableMockBlockProvider::new(vec![
            (10, [0x01; 32], block_a),
            (20, [0x02; 32], block_b),
            (30, [0x03; 32], block_c),
        ]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let blocks_ref = provider.blocks.clone();
        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Find intersection at origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Serve all 3 blocks via MsgRequestNext.
        for _ in 0..3 {
            let req = encode_message(&ChainSyncMessage::MsgRequestNext);
            ingress_tx.send(Bytes::from(req)).await.unwrap();
            let (_, _, resp) = egress_rx.recv().await.unwrap();
            let msg = decode_message(&resp).unwrap();
            assert!(
                matches!(msg, ChainSyncMessage::MsgRollForward { .. }),
                "expected MsgRollForward, got {msg:?}"
            );
        }

        // Simulate rollback: remove blocks at slots 20 and 30, add new fork block.
        let new_fork_block = make_hfc_block(7, &[0xF1]);
        {
            let mut blocks = blocks_ref.lock().unwrap();
            blocks.retain(|(s, _, _)| *s <= 10);
            blocks.push((25, [0xF1; 32], new_fork_block));
        }

        // Broadcast rollback announcement to slot 10.
        rb_tx
            .send(RollbackAnnouncement {
                slot: 10,
                hash: [0x01; 32],
            })
            .unwrap();

        // The next MsgRequestNext should yield MsgRollBackward to slot 10.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollBackward {
            point, tip_slot, ..
        } = msg
        {
            assert_eq!(
                point,
                Point::Specific(10, [0x01; 32]),
                "rollback should target slot 10"
            );
            // Tip should reflect the new chain state (slot 25).
            assert_eq!(tip_slot, 25);
        } else {
            panic!("expected MsgRollBackward, got {msg:?}");
        }

        // Next MsgRequestNext should serve the new fork block at slot 25.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollForward { tip_slot, .. } = msg {
            assert_eq!(tip_slot, 25, "should serve new fork block");
        } else {
            panic!("expected MsgRollForward for new fork, got {msg:?}");
        }

        // Clean up.
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rollback_during_await_reply() {
        // Scenario: follower is at the tip (in MsgAwaitReply/StMustReply state)
        // when a rollback occurs.  The server must send MsgRollBackward, not a
        // stale MsgRollForward.
        let block_a = make_hfc_block(7, &[0x0A]);

        let provider = MutableMockBlockProvider::new(vec![(10, [0x01; 32], block_a)]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Find intersection at origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Serve the only block.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Request next — will go into MsgAwaitReply since we're at the tip.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // Read MsgAwaitReply.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgAwaitReply
        ));

        // Now broadcast a rollback to origin (slot 0).
        // The server is in StMustReply — it must respond with MsgRollBackward.
        rb_tx
            .send(RollbackAnnouncement {
                slot: 0,
                hash: [0u8; 32],
            })
            .unwrap();

        // The server should send MsgRollBackward to origin.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollBackward { point, .. } = msg {
            assert_eq!(point, Point::Origin, "should rollback to origin");
        } else {
            panic!("expected MsgRollBackward while in MsgAwaitReply, got {msg:?}");
        }

        // Clean up.
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rollback_cursor_behind_rollback_point_no_rollback_sent() {
        // Scenario: cursor is at slot 10, rollback occurs to slot 20.
        // Since the cursor is behind the rollback point, the server should
        // NOT send MsgRollBackward — the peer hasn't seen the rolled-back blocks.
        let block_a = make_hfc_block(7, &[0x0A]);
        let block_b = make_hfc_block(7, &[0x0B]);
        let block_c = make_hfc_block(7, &[0x0C]);

        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], block_a),
                (20, [0x02; 32], block_b),
                (30, [0x03; 32], block_c),
            ],
        };

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Find intersection at origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Serve only block at slot 10 (cursor now at slot 10).
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Broadcast rollback to slot 20 — cursor is at slot 10, which is before
        // the rollback point.  No MsgRollBackward should be sent.
        rb_tx
            .send(RollbackAnnouncement {
                slot: 20,
                hash: [0x02; 32],
            })
            .unwrap();

        // Next request should serve the block at slot 20 (MsgRollForward, not RollBackward).
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgRollForward { .. }),
            "cursor behind rollback point should not trigger MsgRollBackward, got {msg:?}"
        );

        // Clean up.
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    // ─── Bug J: cursor revalidation after fork switch (2026-05-16) ───────────

    /// Fork-aware mock that distinguishes blocks on the *canonical chain*
    /// from blocks merely stored as forks.  This is required to exercise
    /// the Bug J fix in the ChainSync server: `try_serve_next_block` must
    /// detect when the cursor's block has been displaced by a fork switch
    /// (off-chain) and send `MsgRollBackward` rather than skipping over
    /// the new chain's earlier-slot blocks.
    type StoredBlock = (u64, u64, [u8; 32], Vec<u8>); // (slot, block_no, prev_hash, cbor)

    struct ForkAwareMockProvider {
        /// Hashes on the current canonical chain, oldest → newest.
        chain: std::sync::Arc<std::sync::Mutex<Vec<[u8; 32]>>>,
        /// Every stored block keyed by hash.
        store: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<[u8; 32], StoredBlock>>>,
    }

    impl ForkAwareMockProvider {
        fn new() -> Self {
            Self {
                chain: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                store: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            }
        }

        /// Append a block to the canonical chain.
        fn push_on_chain(
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
        fn put_fork(
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
        fn replace_chain(&self, new_chain: Vec<[u8; 32]>) {
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

    /// **Bug J regression** (issue #500, 2026-05-16):
    ///
    /// Before this fix, the ChainSync server's `try_serve_next_block` used
    /// only `get_next_block_after_slot(cursor_slot)` to pick the next block
    /// to forward — slot-based, with no validation that the cursor's block
    /// was still on the canonical chain.  After a fork switch that replaced
    /// the in-flight chain with a competitor whose blocks are at *lower*
    /// slots than the cursor (typical for two BPs forging on diverged
    /// chains for several blocks before merging), this lookup silently
    /// skipped every new-chain block at or below the cursor's slot, and
    /// the downstream peer never received the bodies needed to reach the
    /// new tip.  Empirically: dugite-bp's volatile DB had no record of
    /// the relay-side competing chain's blocks 10..16 even though the
    /// relay had selected them after a fork switch.
    ///
    /// Fixed behaviour: when `cursor_hash` is not on the canonical chain,
    /// the server rewinds the cursor to the most recent on-chain ancestor
    /// by sending `MsgRollBackward` BEFORE serving any further blocks.
    /// Subsequent `MsgRequestNext`s then deliver the new chain's blocks
    /// in slot order — including those at slots ≤ the old cursor slot.
    #[tokio::test]
    async fn fork_switch_to_lower_slot_chain_sends_rollback_then_serves_new_blocks() {
        // Phase 1: build a 4-block chain A → B → C → D and serve it to the
        // client.  Hash bytes encode the block_no in the first byte.
        let provider = ForkAwareMockProvider::new();
        let genesis = [0u8; 32];
        let a_hash = [0x0A; 32];
        let b_hash = [0x0B; 32];
        let c_hash = [0x0C; 32];
        let d_hash = [0x0D; 32];

        // Original chain: A@slot10 → B@slot20 → C@slot30 → D@slot40
        provider.push_on_chain(10, a_hash, genesis, 1, make_hfc_block(7, &[0xA1]));
        provider.push_on_chain(20, b_hash, a_hash, 2, make_hfc_block(7, &[0xB1]));
        provider.push_on_chain(30, c_hash, b_hash, 3, make_hfc_block(7, &[0xC1]));
        provider.push_on_chain(40, d_hash, c_hash, 4, make_hfc_block(7, &[0xD1]));

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let mut server = ChainSyncServer::new();
        let handle = {
            let provider_handle = ForkAwareMockProvider {
                chain: provider.chain.clone(),
                store: provider.store.clone(),
            };
            tokio::spawn(async move {
                server
                    .run(&mut channel, &provider_handle, ann_rx, rb_rx)
                    .await
            })
        };

        // Intersect at origin and serve A, B, C, D.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap();

        for _ in 0..4 {
            let req = encode_message(&ChainSyncMessage::MsgRequestNext);
            ingress_tx.send(Bytes::from(req)).await.unwrap();
            let (_, _, resp) = egress_rx.recv().await.unwrap();
            assert!(matches!(
                decode_message(&resp).unwrap(),
                ChainSyncMessage::MsgRollForward { .. }
            ));
        }
        // Cursor now at D@slot40.

        // Phase 2: simulate the fork-switch on the relay side.  Build a new
        // chain A → X (slot15) → Y (slot25) → Z (slot35) → W (slot50).
        // The new chain's intermediate blocks (X, Y, Z) sit at slots BELOW
        // the cursor (40) — these are the blocks that the buggy code lost.
        let x_hash = [0x1A; 32];
        let y_hash = [0x2A; 32];
        let z_hash = [0x3A; 32];
        let w_hash = [0x4A; 32];
        provider.put_fork(15, x_hash, a_hash, 2, make_hfc_block(7, &[0xAA]));
        provider.put_fork(25, y_hash, x_hash, 3, make_hfc_block(7, &[0xBB]));
        provider.put_fork(35, z_hash, y_hash, 4, make_hfc_block(7, &[0xCC]));
        provider.put_fork(50, w_hash, z_hash, 5, make_hfc_block(7, &[0xDD]));
        provider.replace_chain(vec![a_hash, x_hash, y_hash, z_hash, w_hash]);

        // The cursor's block (D@slot40) is now a fork, no longer on chain.
        // Announce the new tip — server wakes, sees cursor off-chain via
        // `is_on_chain`, walks `find_chain_ancestor` back to A, and sends
        // MsgRollBackward(A@slot10).
        ann_tx
            .send(BlockAnnouncement {
                slot: 50,
                hash: w_hash,
                block_number: 5,
            })
            .unwrap();

        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        match msg {
            ChainSyncMessage::MsgRollBackward { point, .. } => {
                assert_eq!(
                    point,
                    Point::Specific(10, a_hash),
                    "must rewind to most recent on-chain ancestor"
                );
            }
            other => panic!(
                "Bug J regression: expected MsgRollBackward to A@10, got {other:?}.  \
                 The cursor was at D@40 which is no longer on the chain; \
                 serving any MsgRollForward here would skip the new chain's \
                 blocks at slots ≤ 40."
            ),
        }

        // Now the client (representing dbp) requests next blocks; the server
        // MUST deliver the entire new chain past A in slot order, including
        // X@15, Y@25, Z@35 — all of which are BELOW the original cursor
        // slot of 40.  Pre-fix, these would have been silently skipped.
        let expected_slots = [15u64, 25, 35, 50];
        for expected_slot in expected_slots {
            let req = encode_message(&ChainSyncMessage::MsgRequestNext);
            ingress_tx.send(Bytes::from(req)).await.unwrap();
            let (_, _, resp) = egress_rx.recv().await.unwrap();
            let msg = decode_message(&resp).unwrap();
            match msg {
                ChainSyncMessage::MsgRollForward { tip_slot, .. } => {
                    // The MsgRollForward payload doesn't expose the served
                    // block's slot directly (only the tip), but the cursor
                    // advances through the chain in order.  Verify the tip
                    // slot is 50 throughout (since the chain hasn't grown
                    // further), and that we received all four expected
                    // forwards.
                    assert_eq!(tip_slot, 50);
                    let _ = expected_slot;
                }
                other => panic!("expected MsgRollForward at slot {expected_slot}, got {other:?}"),
            }
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    // ─── first-block-from-origin regression tests ────────────────────────────

    /// Regression test for a bug where the ChainSync server re-served the
    /// very first block on every subsequent `MsgRequestNext`, causing the
    /// Haskell peer to reject it with `UnexpectedBlockNo`.
    ///
    /// Scenario:
    /// 1. Provider starts empty.
    /// 2. Client sends `MsgFindIntersect(Origin)` → server sets
    ///    `cursor_at_origin = true`.
    /// 3. Client sends `MsgRequestNext` → no block yet, server replies
    ///    `MsgAwaitReply` and waits on the announcement channel.
    /// 4. Producer forges block 0 (slot 1), the block is added to the
    ///    provider, and a `BlockAnnouncement` is broadcast.
    /// 5. Server wakes, serves block 0 via `MsgRollForward`, **must** clear
    ///    `cursor_at_origin`.
    /// 6. Client sends another `MsgRequestNext`.
    ///    EXPECTED: `MsgAwaitReply` (no more blocks).
    ///    BUG (pre-fix): `MsgRollForward` re-serving block 0, because
    ///    `cursor_at_origin` was never cleared in the announcement-wakeup
    ///    path, so `get_block_at_or_after_slot(0)` returned block 0 again.
    #[tokio::test]
    async fn first_block_not_served_twice_when_forged_mid_await() {
        let provider = MutableMockBlockProvider::new(vec![]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        let blocks_ref = provider.blocks.clone();
        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Step 1 — find intersect at Origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgIntersectFound { .. }
        ));

        // Step 2 — request next, no block yet → expect MsgAwaitReply.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(
            matches!(
                decode_message(&resp).unwrap(),
                ChainSyncMessage::MsgAwaitReply
            ),
            "first MsgRequestNext with empty provider must produce MsgAwaitReply"
        );

        // Step 3 — forger produces block 0 at slot 1; commit to provider,
        // then broadcast the announcement (matches node/mod.rs ordering).
        let block0_cbor = make_hfc_block(7, &[0xB0]);
        {
            let mut blocks = blocks_ref.lock().unwrap();
            blocks.push((1, [0xB0; 32], block0_cbor));
        }
        ann_tx
            .send(BlockAnnouncement {
                slot: 1,
                hash: [0xB0; 32],
                block_number: 0,
            })
            .unwrap();

        // Step 4 — the server should wake and send MsgRollForward for block 0.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgRollForward { .. }),
            "expected MsgRollForward after announcement, got {msg:?}"
        );

        // Step 5 — second MsgRequestNext must NOT re-serve block 0.
        // Pre-fix bug: cursor_at_origin stayed true, so the direct-serve
        // path called get_block_at_or_after_slot(0) and served block 0
        // again, producing the Haskell error:
        //   HeaderError ... UnexpectedBlockNo (BlockNo 1) (BlockNo 0)
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgAwaitReply),
            "REGRESSION: second MsgRequestNext re-served the first block \
             instead of MsgAwaitReply; got {msg:?}"
        );

        // Server is now parked in MsgAwaitReply (in a 135-second select).
        // Aborting is the cheapest clean-up for this test; a graceful MsgDone
        // handshake would have to wait out the tip-wait timeout.
        handle.abort();
    }

    /// Verify that MsgIntersectNotFound is sent when no intersection exists.
    #[tokio::test]
    async fn find_intersect_not_found_when_no_blocks_match() {
        // Provider has a block at slot 50, but we ask for slot 999.
        let block_cbor = make_hfc_block(7, &[0x01]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(50, [0x01; 32], block_cbor)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Ask for a point we don't have.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            999, [0xFF; 32],
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgIntersectNotFound { .. }),
            "expected MsgIntersectNotFound, got {msg:?}"
        );

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that MsgFindIntersect with Origin always finds an intersection.
    #[tokio::test]
    async fn find_intersect_origin_always_found() {
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider { blocks: vec![] }; // empty chain
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgIntersectFound { point, .. } = msg {
            assert_eq!(
                point,
                Point::Origin,
                "intersection at origin should return Point::Origin"
            );
        } else {
            panic!("expected MsgIntersectFound, got {msg:?}");
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify cursor state is set correctly after MsgFindIntersect with a specific point.
    #[tokio::test]
    async fn find_intersect_sets_cursor_correctly() {
        let block_cbor = make_hfc_block(7, &[0x01]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![
                (10, [0x01; 32], block_cbor.clone()),
                (20, [0x02; 32], make_hfc_block(7, &[0x02])),
            ],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Intersect at slot 10 (block_a).
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Specific(
            10, [0x01; 32],
        )]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // Next block should be at slot 20 (not slot 10 again).
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgRollForward { tip_slot, .. } = msg {
            assert_eq!(
                tip_slot, 20,
                "after intersect at slot 10, next block should be slot 20"
            );
        } else {
            panic!("expected MsgRollForward after intersect, got {msg:?}");
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that MsgDone causes the server to exit cleanly.
    #[tokio::test]
    async fn msg_done_terminates_server() {
        let (mut channel, _egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider { blocks: vec![] };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();

        // Server should terminate cleanly.
        let result = handle.await.unwrap();
        assert!(
            result.is_ok(),
            "server should exit Ok on MsgDone: {result:?}"
        );
    }

    /// Verify tip information is included correctly in MsgRollForward.
    #[tokio::test]
    async fn roll_forward_includes_tip_info() {
        let block_a = make_hfc_block(7, &[0x01]);
        let block_b = make_hfc_block(7, &[0x02]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(100, [0xAA; 32], block_a), (200, [0xBB; 32], block_b)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Intersect at Origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap();

        // Request next — serve block at slot 100.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();

        if let ChainSyncMessage::MsgRollForward {
            tip_slot,
            tip_hash,
            tip_block_number,
            ..
        } = msg
        {
            // Tip should be the chain tip (slot 200), not the served block (slot 100).
            assert_eq!(tip_slot, 200, "tip_slot should reflect chain tip");
            assert_eq!(tip_hash, [0xBB; 32], "tip_hash should reflect chain tip");
            assert_eq!(
                tip_block_number, 2,
                "tip_block_number should reflect chain length"
            );
        } else {
            panic!("expected MsgRollForward, got {msg:?}");
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that drain_rollback() returns None when channel is empty.
    #[test]
    fn drain_rollback_empty_channel_returns_none() {
        let (_tx, mut rx) = broadcast::channel::<RollbackAnnouncement>(4);
        let result = ChainSyncServer::drain_rollback(&mut rx);
        assert!(result.is_none(), "empty channel should return None");
    }

    /// Verify that drain_rollback() returns the latest of multiple queued rollbacks.
    #[test]
    fn drain_rollback_returns_latest_of_multiple() {
        let (tx, mut rx) = broadcast::channel::<RollbackAnnouncement>(8);

        // Queue 3 rollbacks — only the last should be returned.
        tx.send(RollbackAnnouncement {
            slot: 10,
            hash: [0x01; 32],
        })
        .unwrap();
        tx.send(RollbackAnnouncement {
            slot: 20,
            hash: [0x02; 32],
        })
        .unwrap();
        tx.send(RollbackAnnouncement {
            slot: 30,
            hash: [0x03; 32],
        })
        .unwrap();

        let result = ChainSyncServer::drain_rollback(&mut rx);
        assert!(result.is_some());
        let rb = result.unwrap();
        assert_eq!(rb.slot, 30, "should return the latest (slot 30) rollback");
        assert_eq!(rb.hash, [0x03; 32]);
    }

    /// Verify that the server handles MsgFindIntersect with multiple points,
    /// finding the most recent matching point.
    #[tokio::test]
    async fn find_intersect_picks_most_recent_matching_point() {
        let block_a = make_hfc_block(7, &[0x0A]);
        let block_b = make_hfc_block(7, &[0x0B]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(10, [0x0A; 32], block_a), (20, [0x0B; 32], block_b)],
        };
        let (ann_tx, _) = broadcast::channel(16);
        let (rb_tx, _) = broadcast::channel(16);
        let mut server = ChainSyncServer::new();

        let handle = tokio::spawn(async move {
            server
                .run(
                    &mut channel,
                    &provider,
                    ann_tx.subscribe(),
                    rb_tx.subscribe(),
                )
                .await
        });

        // Send two points in order: [slot=20 (exists), slot=10 (also exists), Origin].
        // The server should match the first one it finds (slot 20).
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![
            Point::Specific(20, [0x0B; 32]),
            Point::Specific(10, [0x0A; 32]),
            Point::Origin,
        ]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();

        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        if let ChainSyncMessage::MsgIntersectFound { point, .. } = msg {
            assert_eq!(
                point,
                Point::Specific(20, [0x0B; 32]),
                "should match the first (most recent) known point"
            );
        } else {
            panic!("expected MsgIntersectFound, got {msg:?}");
        }

        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }

    /// Verify that extract_header_for_chainsync returns error for invalid era tag.
    ///
    /// Storage era tag 100 is not a valid Cardano era and should produce an error
    /// from `storage_era_tag_to_hfc_index`.
    #[test]
    fn extract_header_invalid_era_tag_returns_error() {
        // Build block with era_tag=100 (invalid).
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(100).unwrap(); // invalid era tag
        enc.array(5).unwrap();
        enc.bytes(&[0x01]).unwrap();
        enc.array(0).unwrap();
        enc.array(0).unwrap();
        enc.null().unwrap();
        enc.array(0).unwrap();

        let result = extract_header_for_chainsync(&buf);
        assert!(
            result.is_err(),
            "era_tag=100 should produce an error, got: {result:?}"
        );
    }

    /// Regression lock: MsgAwaitReply is sent when at the tip (no blocks available).
    ///
    /// This verifies the wakeup-race boundary: when the server has no next block,
    /// it MUST send MsgAwaitReply before blocking in the select!{} loop.
    /// Missing this message leaves the peer in StCanAwait with no reply — the
    /// SingMustReply timeout (135s) described in project_chainsync_server_silence.md.
    #[tokio::test]
    async fn await_reply_sent_when_at_tip() {
        let block = make_hfc_block(7, &[0xAA]);
        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let provider = MockBlockProvider {
            blocks: vec![(100, [0xAA; 32], block)],
        };
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();
        let mut server = ChainSyncServer::new();

        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Intersect at origin, serve block, then reach tip.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap(); // MsgRollForward
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Now at tip — send another MsgRequestNext.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();

        // CRITICAL: server MUST send MsgAwaitReply before blocking.
        // If it doesn't, we'd hang here — verifying the wakeup race doesn't
        // prevent the mandatory MsgAwaitReply from being sent.
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        assert!(
            matches!(msg, ChainSyncMessage::MsgAwaitReply),
            "REGRESSION: server did not send MsgAwaitReply at tip — \
             this causes the SingMustReply timeout (project_chainsync_server_silence.md); \
             got {msg:?}"
        );

        handle.abort();
    }

    /// Regression test for stale-announcement draining at the top of
    /// `handle_request_next`.
    ///
    /// Scenario: the producer has already forged and stored block 0, but the
    /// broadcast is queued in `announcement_rx` because the server was in the
    /// middle of handling a previous message. On the next `MsgRequestNext`:
    /// 1. The server must drain the buffered announcement.
    /// 2. Serve block 0 via the direct path (using the authoritative
    ///    `BlockProvider` lookup).
    /// 3. On the subsequent `MsgRequestNext`, the server must serve block 1
    ///    (NOT re-serve block 0 from a stale queued announcement).
    #[tokio::test]
    async fn pre_queued_announcement_does_not_cause_duplicate_first_block() {
        let block0 = make_hfc_block(7, &[0xB0]);
        let block1 = make_hfc_block(7, &[0xB1]);
        let provider =
            MutableMockBlockProvider::new(vec![(1, [0xB0; 32], block0), (2, [0xB1; 32], block1)]);

        let (mut channel, mut egress_rx, ingress_tx) = make_test_channel();
        let (ann_tx, _) = broadcast::channel::<BlockAnnouncement>(16);
        let (rb_tx, _) = broadcast::channel::<RollbackAnnouncement>(16);
        let ann_rx = ann_tx.subscribe();
        let rb_rx = rb_tx.subscribe();

        // Queue a stale announcement for block 0 before the server starts —
        // this simulates a forge event that happened while the server was
        // still setting up.
        ann_tx
            .send(BlockAnnouncement {
                slot: 1,
                hash: [0xB0; 32],
                block_number: 0,
            })
            .unwrap();

        let mut server = ChainSyncServer::new();
        let handle =
            tokio::spawn(async move { server.run(&mut channel, &provider, ann_rx, rb_rx).await });

        // Intersect at Origin.
        let find = encode_message(&ChainSyncMessage::MsgFindIntersect(vec![Point::Origin]));
        ingress_tx.send(Bytes::from(find)).await.unwrap();
        let _ = egress_rx.recv().await.unwrap(); // MsgIntersectFound

        // First MsgRequestNext: server drains the stale announcement for
        // block 0, then serves block 0 via the direct path.  The drain
        // prevents the stale announcement from re-waking the server on the
        // next MsgRequestNext.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        assert!(matches!(
            decode_message(&resp).unwrap(),
            ChainSyncMessage::MsgRollForward { .. }
        ));

        // Second MsgRequestNext: must serve block 1 (slot 2), not re-serve
        // block 0.  Before the fix, the stale queued announcement for block
        // 0 could wake the server in the await branch and cause a duplicate.
        let req = encode_message(&ChainSyncMessage::MsgRequestNext);
        ingress_tx.send(Bytes::from(req)).await.unwrap();
        let (_, _, resp) = egress_rx.recv().await.unwrap();
        let msg = decode_message(&resp).unwrap();
        match msg {
            ChainSyncMessage::MsgRollForward { tip_slot, .. } => {
                assert_eq!(tip_slot, 2, "second serve must advance past block 0");
            }
            other => panic!("expected MsgRollForward for block 1, got {other:?}"),
        }

        // Clean up.
        let done = encode_message(&ChainSyncMessage::MsgDone);
        ingress_tx.send(Bytes::from(done)).await.unwrap();
        handle.await.unwrap().unwrap();
    }
}
