use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

use crate::multiplexer::Segment;
use crate::query_handler::{QueryHandler, QueryResult};

#[derive(Error, Debug)]
pub enum N2CServerError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Handshake failed: {0}")]
    HandshakeFailed(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
}

/// N2C mini-protocol IDs
const MINI_PROTOCOL_HANDSHAKE: u16 = 0;
const MINI_PROTOCOL_CHAINSYNC: u16 = 5;
const MINI_PROTOCOL_TX_SUBMISSION: u16 = 6;
const MINI_PROTOCOL_STATE_QUERY: u16 = 7;

/// Node-to-Client server that listens on a Unix domain socket.
pub struct N2CServer {
    query_handler: Arc<RwLock<QueryHandler>>,
}

impl N2CServer {
    pub fn new(query_handler: Arc<RwLock<QueryHandler>>) -> Self {
        N2CServer { query_handler }
    }

    /// Start listening on the given Unix socket path.
    /// This runs indefinitely, accepting connections and spawning tasks for each.
    pub async fn listen(&self, socket_path: &Path) -> Result<(), N2CServerError> {
        // Remove existing socket file if present
        if socket_path.exists() {
            std::fs::remove_file(socket_path)?;
        }

        let listener = UnixListener::bind(socket_path)?;
        info!("N2C server listening on {}", socket_path.display());

        loop {
            match listener.accept().await {
                Ok((stream, _addr)) => {
                    info!("N2C client connected");
                    let handler = self.query_handler.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle_n2c_connection(stream, handler).await {
                            warn!("N2C connection error: {e}");
                        }
                        debug!("N2C client disconnected");
                    });
                }
                Err(e) => {
                    error!("Failed to accept N2C connection: {e}");
                }
            }
        }
    }
}

/// Handle a single N2C client connection
async fn handle_n2c_connection(
    mut stream: tokio::net::UnixStream,
    query_handler: Arc<RwLock<QueryHandler>>,
) -> Result<(), N2CServerError> {
    let mut buf = vec![0u8; 65536];

    loop {
        let n = stream.read(&mut buf).await?;
        if n == 0 {
            return Ok(()); // Client disconnected
        }

        // Parse multiplexer segments from the received data
        let mut offset = 0;
        while offset < n {
            let remaining = &buf[offset..n];
            if remaining.len() < 8 {
                break; // Need more data for a complete header
            }

            match Segment::decode(remaining) {
                Ok((segment, consumed)) => {
                    offset += consumed;

                    // Process the segment
                    let response = process_segment(&segment, &query_handler).await?;
                    if let Some(resp_segment) = response {
                        let encoded = resp_segment.encode();
                        stream.write_all(&encoded).await?;
                    }
                }
                Err(_) => {
                    break; // Incomplete segment
                }
            }
        }
    }
}

/// Process a single multiplexer segment and optionally return a response
async fn process_segment(
    segment: &Segment,
    query_handler: &Arc<RwLock<QueryHandler>>,
) -> Result<Option<Segment>, N2CServerError> {
    match segment.protocol_id {
        MINI_PROTOCOL_HANDSHAKE => handle_handshake(&segment.payload),
        MINI_PROTOCOL_STATE_QUERY => handle_state_query(&segment.payload, query_handler).await,
        MINI_PROTOCOL_TX_SUBMISSION => {
            debug!("LocalTxSubmission message received (not yet implemented)");
            Ok(None)
        }
        MINI_PROTOCOL_CHAINSYNC => {
            debug!("LocalChainSync message received (not yet implemented)");
            Ok(None)
        }
        other => {
            debug!("Unknown N2C mini-protocol: {other}");
            Ok(None)
        }
    }
}

/// Handle N2C handshake
///
/// N2C handshake proposes versions. We accept the highest version we support.
/// The CBOR format is: [0, { version_number: params, ... }] for propose
/// We respond with: [1, version_number, params] for accept
fn handle_handshake(payload: &[u8]) -> Result<Option<Segment>, N2CServerError> {
    // Parse CBOR handshake proposal
    // The client sends [0, {version -> params}]
    // We need to find the highest version we support and accept it

    // For now, try to decode and accept a reasonable version
    // Simple handshake: accept version 16 (Conway) with network magic
    // Response: [1, version, [network_magic, false]]
    let mut response_buf = Vec::new();
    let mut encoder = minicbor::Encoder::new(&mut response_buf);

    // Try to parse the proposed versions to extract network magic
    let network_magic = parse_handshake_magic(payload).unwrap_or(764824073); // mainnet default
    let version = parse_highest_version(payload).unwrap_or(16);

    debug!("N2C handshake: accepting version {version}, magic {network_magic}");

    // Encode accept response: [1, version, [magic, false]]
    encoder
        .array(3)
        .map_err(|e| N2CServerError::Protocol(e.to_string()))?;
    encoder
        .u32(1)
        .map_err(|e| N2CServerError::Protocol(e.to_string()))?; // MsgAcceptVersion
    encoder
        .u32(version as u32)
        .map_err(|e| N2CServerError::Protocol(e.to_string()))?;
    encoder
        .array(2)
        .map_err(|e| N2CServerError::Protocol(e.to_string()))?;
    encoder
        .u64(network_magic)
        .map_err(|e| N2CServerError::Protocol(e.to_string()))?;
    encoder
        .bool(false)
        .map_err(|e| N2CServerError::Protocol(e.to_string()))?; // query mode = false

    Ok(Some(Segment {
        transmission_time: 0,
        protocol_id: MINI_PROTOCOL_HANDSHAKE,
        is_responder: true,
        payload: response_buf,
    }))
}

/// Parse the network magic from a handshake proposal
fn parse_handshake_magic(payload: &[u8]) -> Option<u64> {
    let mut decoder = minicbor::Decoder::new(payload);
    // [0, { version: [magic, query] }]
    decoder.array().ok()?;
    decoder.u32().ok()?; // msg type = 0 (propose)
    let map_len = decoder.map().ok()?;
    if map_len == Some(0) {
        return None;
    }
    decoder.u32().ok()?; // first version number
                         // Value is either [magic, query] or just magic
    if let Ok(Some(_arr_len)) = decoder.array() {
        decoder.u64().ok()
    } else {
        None
    }
}

/// Parse the highest proposed version number
fn parse_highest_version(payload: &[u8]) -> Option<u16> {
    let mut decoder = minicbor::Decoder::new(payload);
    decoder.array().ok()?;
    decoder.u32().ok()?; // msg type
    let map_len = decoder.map().ok()??;
    let mut highest = 0u16;
    for _ in 0..map_len {
        if let Ok(v) = decoder.u32() {
            if v as u16 > highest && v <= 17 {
                highest = v as u16;
            }
        }
        // Skip the value (params)
        decoder.skip().ok()?;
    }
    if highest > 0 {
        Some(highest)
    } else {
        None
    }
}

/// Handle LocalStateQuery messages
///
/// Protocol flow:
///   Client: MsgAcquire(point) → Server: MsgAcquired
///   Client: MsgQuery(query)   → Server: MsgResult(result)
///   Client: MsgRelease        → (back to idle)
///   Client: MsgDone           → (end)
async fn handle_state_query(
    payload: &[u8],
    query_handler: &Arc<RwLock<QueryHandler>>,
) -> Result<Option<Segment>, N2CServerError> {
    let mut decoder = minicbor::Decoder::new(payload);

    // Parse the CBOR message tag
    let msg_tag = match decoder.array() {
        Ok(Some(len)) if len >= 1 => decoder
            .u32()
            .map_err(|e| N2CServerError::Protocol(format!("bad msg tag: {e}")))?,
        Ok(None) => {
            // Indefinite length array
            decoder
                .u32()
                .map_err(|e| N2CServerError::Protocol(format!("bad msg tag: {e}")))?
        }
        _ => {
            return Err(N2CServerError::Protocol(
                "invalid state query message".into(),
            ))
        }
    };

    match msg_tag {
        0 => {
            // MsgAcquire(point)
            debug!("LocalStateQuery: MsgAcquire");
            // Respond with MsgAcquired [1]
            let mut resp = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut resp);
            enc.array(1)
                .map_err(|e| N2CServerError::Protocol(e.to_string()))?;
            enc.u32(1)
                .map_err(|e| N2CServerError::Protocol(e.to_string()))?; // MsgAcquired
            Ok(Some(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_STATE_QUERY,
                is_responder: true,
                payload: resp,
            }))
        }
        3 => {
            // MsgQuery(query)
            debug!("LocalStateQuery: MsgQuery");
            let handler = query_handler.read().await;
            let result = handler.handle_query_cbor(payload);
            let response_cbor = encode_query_result(&result);

            Ok(Some(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_STATE_QUERY,
                is_responder: true,
                payload: response_cbor,
            }))
        }
        5 => {
            // MsgReAcquire(point)
            debug!("LocalStateQuery: MsgReAcquire");
            // Respond with MsgAcquired [1]
            let mut resp = Vec::new();
            let mut enc = minicbor::Encoder::new(&mut resp);
            enc.array(1)
                .map_err(|e| N2CServerError::Protocol(e.to_string()))?;
            enc.u32(1)
                .map_err(|e| N2CServerError::Protocol(e.to_string()))?;
            Ok(Some(Segment {
                transmission_time: 0,
                protocol_id: MINI_PROTOCOL_STATE_QUERY,
                is_responder: true,
                payload: resp,
            }))
        }
        7 => {
            // MsgRelease
            debug!("LocalStateQuery: MsgRelease");
            Ok(None)
        }
        9 => {
            // MsgDone
            debug!("LocalStateQuery: MsgDone");
            Ok(None)
        }
        other => {
            warn!("Unknown LocalStateQuery message tag: {other}");
            Ok(None)
        }
    }
}

/// Encode a QueryResult into a MsgResult CBOR response
fn encode_query_result(result: &QueryResult) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);

    // MsgResult [4, result]
    enc.array(2).ok();
    enc.u32(4).ok(); // MsgResult tag

    match result {
        QueryResult::EpochNo(epoch) => {
            enc.u64(*epoch).ok();
        }
        QueryResult::ChainTip {
            slot,
            hash,
            block_no,
        } => {
            enc.array(2).ok();
            // Point: [slot, hash]
            enc.array(2).ok();
            enc.u64(*slot).ok();
            enc.bytes(hash).ok();
            // Block number
            enc.u64(*block_no).ok();
        }
        QueryResult::CurrentEra(era) => {
            enc.u32(*era).ok();
        }
        QueryResult::SystemStart(time_str) => {
            enc.str(time_str).ok();
        }
        QueryResult::ChainBlockNo(block_no) => {
            enc.u64(*block_no).ok();
        }
        QueryResult::ProtocolParams(cbor) => {
            enc.bytes(cbor).ok();
        }
        QueryResult::StakeDistribution(pools) => {
            enc.map(pools.len() as u64).ok();
            for (pool_id, stake) in pools {
                enc.bytes(pool_id).ok();
                enc.u64(*stake).ok();
            }
        }
        QueryResult::Error(msg) => {
            enc.str(msg).ok();
        }
    }

    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_highest_version_basic() {
        // Encode a handshake proposal: [0, {1: [764824073, false], 16: [764824073, false]}]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(0).unwrap(); // MsgProposeVersions
        enc.map(2).unwrap();
        enc.u32(1).unwrap();
        enc.array(2).unwrap();
        enc.u64(764824073).unwrap();
        enc.bool(false).unwrap();
        enc.u32(16).unwrap();
        enc.array(2).unwrap();
        enc.u64(764824073).unwrap();
        enc.bool(false).unwrap();

        assert_eq!(parse_highest_version(&buf), Some(16));
    }

    #[test]
    fn test_parse_handshake_magic() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(0).unwrap();
        enc.map(1).unwrap();
        enc.u32(16).unwrap();
        enc.array(2).unwrap();
        enc.u64(1).unwrap(); // preview testnet magic
        enc.bool(false).unwrap();

        assert_eq!(parse_handshake_magic(&buf), Some(1));
    }

    #[test]
    fn test_encode_query_result_epoch() {
        let result = QueryResult::EpochNo(500);
        let cbor = encode_query_result(&result);
        assert!(!cbor.is_empty());
    }

    #[test]
    fn test_encode_query_result_chain_tip() {
        let result = QueryResult::ChainTip {
            slot: 12345,
            hash: vec![0u8; 32],
            block_no: 100,
        };
        let cbor = encode_query_result(&result);
        assert!(!cbor.is_empty());
    }
}
