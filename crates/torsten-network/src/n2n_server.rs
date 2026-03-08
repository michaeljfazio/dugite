use std::net::SocketAddr;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::multiplexer::Segment;
use crate::query_handler::QueryHandler;

/// Callback trait for retrieving block data from storage
pub trait BlockProvider: Send + Sync + 'static {
    /// Get raw CBOR block bytes by header hash
    fn get_block(&self, hash: &[u8; 32]) -> Option<Vec<u8>>;
    /// Check if a block exists
    fn has_block(&self, hash: &[u8; 32]) -> bool;
    /// Get the current chain tip (slot, hash, block_number)
    fn get_tip(&self) -> (u64, [u8; 32], u64);
}

#[derive(Error, Debug)]
pub enum N2NServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Handshake failed: {0}")]
    HandshakeFailed(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
}

/// N2N mini-protocol IDs
const MINI_PROTOCOL_HANDSHAKE: u16 = 0;
const MINI_PROTOCOL_CHAINSYNC: u16 = 2;
const MINI_PROTOCOL_BLOCKFETCH: u16 = 3;
const MINI_PROTOCOL_KEEPALIVE: u16 = 8;

/// Node-to-Node server that accepts inbound TCP connections from remote peers.
pub struct N2NServer {
    listen_addr: SocketAddr,
    network_magic: u64,
    query_handler: Arc<RwLock<QueryHandler>>,
    block_provider: Arc<dyn BlockProvider>,
    max_connections: usize,
}

impl N2NServer {
    pub fn new(
        listen_addr: SocketAddr,
        network_magic: u64,
        query_handler: Arc<RwLock<QueryHandler>>,
        block_provider: Arc<dyn BlockProvider>,
        max_connections: usize,
    ) -> Self {
        N2NServer {
            listen_addr,
            network_magic,
            query_handler,
            block_provider,
            max_connections,
        }
    }

    /// Start listening for inbound N2N connections.
    pub async fn listen(&self) -> Result<(), N2NServerError> {
        let listener = TcpListener::bind(self.listen_addr).await?;
        info!("N2N server listening on {}", self.listen_addr);

        let active_connections = Arc::new(std::sync::atomic::AtomicUsize::new(0));

        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    let active = active_connections.load(std::sync::atomic::Ordering::Relaxed);
                    if active >= self.max_connections {
                        warn!(
                            peer = %peer_addr,
                            active,
                            max = self.max_connections,
                            "Rejecting connection: max connections reached"
                        );
                        drop(stream);
                        continue;
                    }

                    active_connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    info!(peer = %peer_addr, "N2N peer connected");

                    let query_handler = self.query_handler.clone();
                    let block_provider = self.block_provider.clone();
                    let network_magic = self.network_magic;
                    let counter = active_connections.clone();

                    tokio::spawn(async move {
                        if let Err(e) = handle_n2n_connection(
                            stream,
                            peer_addr,
                            network_magic,
                            query_handler,
                            block_provider,
                        )
                        .await
                        {
                            debug!(peer = %peer_addr, "N2N connection ended: {e}");
                        }
                        counter.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        info!(peer = %peer_addr, "N2N peer disconnected");
                    });
                }
                Err(e) => {
                    error!("Failed to accept N2N connection: {e}");
                }
            }
        }
    }
}

/// Handle a single inbound N2N peer connection
async fn handle_n2n_connection(
    mut stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    network_magic: u64,
    query_handler: Arc<RwLock<QueryHandler>>,
    block_provider: Arc<dyn BlockProvider>,
) -> Result<(), N2NServerError> {
    let mut buf = vec![0u8; 65536];
    let mut partial = Vec::new();

    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(()); // Peer disconnected
        }

        partial.extend_from_slice(&buf[..n]);

        // Process all complete segments
        let mut offset = 0;
        while offset < partial.len() {
            let remaining = &partial[offset..];
            if remaining.len() < 8 {
                break;
            }

            match Segment::decode(remaining) {
                Ok((segment, consumed)) => {
                    offset += consumed;

                    let response = process_n2n_segment(
                        &segment,
                        peer_addr,
                        network_magic,
                        &query_handler,
                        &block_provider,
                    )
                    .await?;

                    for resp in response {
                        let encoded = resp.encode();
                        stream.write_all(&encoded).await?;
                    }
                }
                Err(_) => {
                    break; // Incomplete segment, wait for more data
                }
            }
        }

        // Keep any unprocessed data
        if offset > 0 {
            partial.drain(..offset);
        }
    }
}

/// Process a single N2N multiplexer segment
async fn process_n2n_segment(
    segment: &Segment,
    peer_addr: SocketAddr,
    network_magic: u64,
    query_handler: &Arc<RwLock<QueryHandler>>,
    block_provider: &Arc<dyn BlockProvider>,
) -> Result<Vec<Segment>, N2NServerError> {
    match segment.protocol_id {
        MINI_PROTOCOL_HANDSHAKE => {
            let resp = handle_n2n_handshake(&segment.payload, network_magic)?;
            Ok(resp.into_iter().collect())
        }
        MINI_PROTOCOL_CHAINSYNC => {
            let resp =
                handle_n2n_chainsync(&segment.payload, query_handler, block_provider).await?;
            Ok(resp.into_iter().collect())
        }
        MINI_PROTOCOL_BLOCKFETCH => {
            let resp = handle_n2n_blockfetch(&segment.payload, block_provider)?;
            Ok(resp)
        }
        MINI_PROTOCOL_KEEPALIVE => {
            let resp = handle_keepalive(&segment.payload)?;
            Ok(resp.into_iter().collect())
        }
        other => {
            debug!(peer = %peer_addr, protocol = other, "Unknown N2N mini-protocol");
            Ok(vec![])
        }
    }
}

/// Handle N2N version handshake.
///
/// N2N handshake format:
///   Client sends: [0, { version: params, ... }] (MsgProposeVersions)
///   Server responds: [1, version, params] (MsgAcceptVersion)
///   Or: [2, reason] (MsgRefuse)
fn handle_n2n_handshake(
    payload: &[u8],
    network_magic: u64,
) -> Result<Option<Segment>, N2NServerError> {
    let mut decoder = minicbor::Decoder::new(payload);

    // Parse [tag, versions_map]
    let _arr_len = decoder
        .array()
        .map_err(|e| N2NServerError::HandshakeFailed(e.to_string()))?;
    let msg_tag = decoder
        .u32()
        .map_err(|e| N2NServerError::HandshakeFailed(e.to_string()))?;

    if msg_tag != 0 {
        return Err(N2NServerError::HandshakeFailed(format!(
            "Expected MsgProposeVersions (0), got {msg_tag}"
        )));
    }

    // Parse version map to find the highest version we support
    // N2N versions: 7-14 (Shelley through Conway)
    // We support versions 13-14 (Babbage/Conway)
    let mut best_version: Option<u32> = None;
    let map_len = decoder
        .map()
        .map_err(|e| N2NServerError::HandshakeFailed(e.to_string()))?;

    let count = map_len.unwrap_or(0);
    for _ in 0..count {
        let version = decoder
            .u32()
            .map_err(|e| N2NServerError::HandshakeFailed(e.to_string()))?;
        // Skip the value (params)
        decoder
            .skip()
            .map_err(|e| N2NServerError::HandshakeFailed(e.to_string()))?;

        // Accept versions 13-14 (Babbage and Conway N2N)
        if (13..=14).contains(&version)
            && (best_version.is_none() || version > best_version.unwrap())
        {
            best_version = Some(version);
        }
    }

    let version = match best_version {
        Some(v) => v,
        None => {
            // Refuse: no compatible version
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u32(2)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?; // MsgRefuse
            enc.array(2)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u32(0)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?; // VersionMismatch
            enc.array(0)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?; // empty list

            return Ok(Some(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_HANDSHAKE,
                is_responder: true,
                payload: buf,
            }));
        }
    };

    debug!("N2N handshake: accepting version {version}, magic {network_magic}");

    // Encode MsgAcceptVersion: [1, version, params]
    // N2N V13+ params: [network_magic, initiator_only_diffusion_mode, peer_sharing, query]
    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);
    enc.array(3)
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
    enc.u32(1)
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?; // MsgAcceptVersion
    enc.u32(version)
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

    // Version params: [magic, initiator_only_diffusion_mode, peer_sharing, query]
    enc.array(4)
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
    enc.u64(network_magic)
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
    enc.bool(false)
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?; // initiator_only = false
    enc.u32(0)
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?; // peer_sharing = NoPeerSharing
    enc.bool(false)
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?; // query = false

    Ok(Some(Segment {
        transmission_time: 0,
        protocol_id: MINI_PROTOCOL_HANDSHAKE,
        is_responder: true,
        payload: buf,
    }))
}

/// Handle N2N ChainSync mini-protocol messages.
///
/// As a server (responder), we respond to:
///   MsgRequestNext (0) → MsgRollForward (2) or MsgRollBackward (3) or MsgAwaitReply (1)
///   MsgFindIntersect (4) → MsgIntersectFound (5) or MsgIntersectNotFound (6)
///   MsgDone (7) → close protocol
async fn handle_n2n_chainsync(
    payload: &[u8],
    query_handler: &Arc<RwLock<QueryHandler>>,
    _block_provider: &Arc<dyn BlockProvider>,
) -> Result<Option<Segment>, N2NServerError> {
    let mut decoder = minicbor::Decoder::new(payload);
    let _arr_len = decoder
        .array()
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
    let msg_tag = decoder
        .u32()
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

    match msg_tag {
        // MsgRequestNext → respond with MsgAwaitReply for now
        // (Full implementation would track per-peer cursor and deliver headers)
        0 => {
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            // MsgAwaitReply: [1]
            enc.array(1)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u32(1)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

            Ok(Some(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_CHAINSYNC,
                is_responder: true,
                payload: buf,
            }))
        }
        // MsgFindIntersect → respond with our tip as the intersection
        4 => {
            let handler = query_handler.read().await;
            let state = handler.state();

            let tip_slot = state.tip.point.slot().map(|s| s.0).unwrap_or(0);
            let tip_hash: Vec<u8> = state
                .tip
                .point
                .hash()
                .map(|h| h.as_ref().to_vec())
                .unwrap_or_else(|| vec![0u8; 32]);
            let tip_block = state.block_number;

            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);

            // MsgIntersectFound: [5, point, tip]
            // point: [slot, hash]
            // tip: [point, block_number]
            enc.array(3)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u32(5)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

            // Point
            enc.array(2)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u64(tip_slot)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.bytes(&tip_hash)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

            // Tip: [point, block_number]
            enc.array(2)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.array(2)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u64(tip_slot)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.bytes(&tip_hash)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u64(tip_block.0)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

            Ok(Some(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_CHAINSYNC,
                is_responder: true,
                payload: buf,
            }))
        }
        // MsgDone
        7 => {
            debug!("N2N ChainSync: peer sent MsgDone");
            Ok(None)
        }
        other => {
            warn!("N2N ChainSync: unknown message tag {other}");
            Ok(None)
        }
    }
}

/// Handle N2N BlockFetch mini-protocol messages.
///
///   MsgRequestRange (0) [from_point, to_point] → MsgStartBatch (2) + blocks + MsgBatchDone (5)
///   MsgClientDone (1) → close protocol
fn handle_n2n_blockfetch(
    payload: &[u8],
    block_provider: &Arc<dyn BlockProvider>,
) -> Result<Vec<Segment>, N2NServerError> {
    let mut decoder = minicbor::Decoder::new(payload);
    let _arr_len = decoder
        .array()
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
    let msg_tag = decoder
        .u32()
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

    match msg_tag {
        // MsgRequestRange: [0, from_point, to_point]
        0 => {
            // Parse from_point [slot, hash] and to_point [slot, hash]
            let from_hash = parse_point_hash(&mut decoder);
            let to_hash = parse_point_hash(&mut decoder);

            let mut segments = Vec::new();

            // MsgStartBatch: [2]
            let mut start_buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut start_buf);
            enc.array(1)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u32(2)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            segments.push(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_BLOCKFETCH,
                is_responder: true,
                payload: start_buf,
            });

            // Send blocks — for now, serve the range from/to if we have them
            // This is simplified: we just try to serve each hash
            for hash in [from_hash, to_hash].iter().flatten() {
                if let Some(block_data) = block_provider.get_block(hash) {
                    // MsgBlock: [3, block_bytes]
                    let mut block_buf = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut block_buf);
                    enc.array(2)
                        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
                    enc.u32(3)
                        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
                    enc.bytes(&block_data)
                        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
                    segments.push(Segment {
                        transmission_time: 0,
                        protocol_id: MINI_PROTOCOL_BLOCKFETCH,
                        is_responder: true,
                        payload: block_buf,
                    });
                }
            }

            // MsgBatchDone: [5]
            let mut done_buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut done_buf);
            enc.array(1)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u32(5)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            segments.push(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_BLOCKFETCH,
                is_responder: true,
                payload: done_buf,
            });

            Ok(segments)
        }
        // MsgClientDone
        1 => {
            debug!("N2N BlockFetch: peer sent MsgClientDone");
            Ok(vec![])
        }
        other => {
            warn!("N2N BlockFetch: unknown message tag {other}");
            Ok(vec![])
        }
    }
}

/// Handle KeepAlive mini-protocol.
///
///   MsgKeepAlive (0) [cookie] → MsgKeepAliveResponse (1) [cookie]
///   MsgDone (2) → close protocol
fn handle_keepalive(payload: &[u8]) -> Result<Option<Segment>, N2NServerError> {
    let mut decoder = minicbor::Decoder::new(payload);
    let _arr_len = decoder
        .array()
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
    let msg_tag = decoder
        .u32()
        .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

    match msg_tag {
        // MsgKeepAlive: [0, cookie]
        0 => {
            let cookie = decoder.u16().unwrap_or(0);

            // MsgKeepAliveResponse: [1, cookie]
            let mut buf = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut buf);
            enc.array(2)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u32(1)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;
            enc.u16(cookie)
                .map_err(|e| N2NServerError::Protocol(e.to_string()))?;

            Ok(Some(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_KEEPALIVE,
                is_responder: true,
                payload: buf,
            }))
        }
        // MsgDone
        2 => {
            debug!("KeepAlive: peer sent MsgDone");
            Ok(None)
        }
        other => {
            debug!("KeepAlive: unknown tag {other}");
            Ok(None)
        }
    }
}

/// Parse a point's hash from a CBOR-encoded [slot, hash] array
fn parse_point_hash(decoder: &mut minicbor::Decoder) -> Option<[u8; 32]> {
    decoder.array().ok()?;
    decoder.u64().ok()?; // slot
    let hash_bytes = decoder.bytes().ok()?;
    if hash_bytes.len() == 32 {
        let mut hash = [0u8; 32];
        hash.copy_from_slice(hash_bytes);
        Some(hash)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_handle_n2n_handshake_accept() {
        // Build a MsgProposeVersions: [0, {13: [magic, false, 0, false], 14: [magic, false, 0, false]}]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(0).unwrap(); // MsgProposeVersions
        enc.map(2).unwrap();
        // Version 13
        enc.u32(13).unwrap();
        enc.array(4).unwrap();
        enc.u64(2).unwrap(); // preview magic
        enc.bool(false).unwrap();
        enc.u32(0).unwrap();
        enc.bool(false).unwrap();
        // Version 14
        enc.u32(14).unwrap();
        enc.array(4).unwrap();
        enc.u64(2).unwrap();
        enc.bool(false).unwrap();
        enc.u32(0).unwrap();
        enc.bool(false).unwrap();

        let result = handle_n2n_handshake(&buf, 2).unwrap();
        assert!(result.is_some());
        let seg = result.unwrap();
        assert_eq!(seg.protocol_id, MINI_PROTOCOL_HANDSHAKE);
        assert!(seg.is_responder);

        // Verify response contains MsgAcceptVersion (tag 1) with version 14
        let mut dec = minicbor::Decoder::new(&seg.payload);
        dec.array().unwrap();
        let tag = dec.u32().unwrap();
        assert_eq!(tag, 1); // MsgAcceptVersion
        let version = dec.u32().unwrap();
        assert_eq!(version, 14); // highest supported
    }

    #[test]
    fn test_handle_n2n_handshake_refuse_incompatible() {
        // Propose only version 7 (too old)
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(0).unwrap();
        enc.map(1).unwrap();
        enc.u32(7).unwrap();
        enc.array(1).unwrap();
        enc.u64(764824073).unwrap();

        let result = handle_n2n_handshake(&buf, 764824073).unwrap();
        assert!(result.is_some());
        let seg = result.unwrap();

        let mut dec = minicbor::Decoder::new(&seg.payload);
        dec.array().unwrap();
        let tag = dec.u32().unwrap();
        assert_eq!(tag, 2); // MsgRefuse
    }

    #[test]
    fn test_handle_keepalive_response() {
        // MsgKeepAlive: [0, cookie]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(0).unwrap();
        enc.u16(42).unwrap();

        let result = handle_keepalive(&buf).unwrap();
        assert!(result.is_some());
        let seg = result.unwrap();
        assert_eq!(seg.protocol_id, MINI_PROTOCOL_KEEPALIVE);

        let mut dec = minicbor::Decoder::new(&seg.payload);
        dec.array().unwrap();
        let tag = dec.u32().unwrap();
        assert_eq!(tag, 1); // MsgKeepAliveResponse
        let cookie = dec.u16().unwrap();
        assert_eq!(cookie, 42);
    }

    #[test]
    fn test_handle_keepalive_done() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(1).unwrap();
        enc.u32(2).unwrap(); // MsgDone

        let result = handle_keepalive(&buf).unwrap();
        assert!(result.is_none());
    }

    struct MockBlockProvider;

    impl BlockProvider for MockBlockProvider {
        fn get_block(&self, _hash: &[u8; 32]) -> Option<Vec<u8>> {
            Some(vec![0x82, 0x01, 0x02]) // dummy CBOR
        }
        fn has_block(&self, _hash: &[u8; 32]) -> bool {
            true
        }
        fn get_tip(&self) -> (u64, [u8; 32], u64) {
            (100, [0xAA; 32], 50)
        }
    }

    #[test]
    fn test_handle_blockfetch_request_range() {
        let provider: Arc<dyn BlockProvider> = Arc::new(MockBlockProvider);

        // MsgRequestRange: [0, [slot, hash], [slot, hash]]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(3).unwrap();
        enc.u32(0).unwrap(); // MsgRequestRange
                             // from point
        enc.array(2).unwrap();
        enc.u64(10).unwrap();
        enc.bytes(&[0xBB; 32]).unwrap();
        // to point
        enc.array(2).unwrap();
        enc.u64(20).unwrap();
        enc.bytes(&[0xCC; 32]).unwrap();

        let segments = handle_n2n_blockfetch(&buf, &provider).unwrap();
        // Should have: MsgStartBatch + 2 blocks + MsgBatchDone = 4 segments
        assert_eq!(segments.len(), 4);

        // First segment: MsgStartBatch [2]
        let mut dec = minicbor::Decoder::new(&segments[0].payload);
        dec.array().unwrap();
        assert_eq!(dec.u32().unwrap(), 2);

        // Last segment: MsgBatchDone [5]
        let mut dec = minicbor::Decoder::new(&segments[3].payload);
        dec.array().unwrap();
        assert_eq!(dec.u32().unwrap(), 5);
    }

    #[test]
    fn test_handle_blockfetch_client_done() {
        let provider: Arc<dyn BlockProvider> = Arc::new(MockBlockProvider);

        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(1).unwrap();
        enc.u32(1).unwrap(); // MsgClientDone

        let segments = handle_n2n_blockfetch(&buf, &provider).unwrap();
        assert!(segments.is_empty());
    }
}
