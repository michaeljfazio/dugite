use std::path::Path;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::debug;

use crate::multiplexer::Segment;

#[derive(Error, Debug)]
pub enum N2CClientError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Handshake rejected")]
    HandshakeRejected,
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Timeout")]
    Timeout,
}

/// N2C mini-protocol IDs
const MINI_PROTOCOL_HANDSHAKE: u16 = 0;
const MINI_PROTOCOL_STATE_QUERY: u16 = 7;

/// Node-to-Client client for connecting to a Cardano node via Unix socket.
pub struct N2CClient {
    stream: UnixStream,
}

/// Result of a tip query
#[derive(Debug, Clone)]
pub struct TipResult {
    pub slot: u64,
    pub hash: Vec<u8>,
    pub block_no: u64,
    pub epoch: u64,
    pub era: u32,
}

/// Result of a generic query
#[derive(Debug, Clone)]
pub enum LocalQueryResult {
    Tip(TipResult),
    EpochNo(u64),
    Era(u32),
    SystemStart(String),
    BlockNo(u64),
    Raw(Vec<u8>),
    Error(String),
}

impl N2CClient {
    /// Connect to a node's Unix domain socket
    pub async fn connect(socket_path: &Path) -> Result<Self, N2CClientError> {
        let stream = UnixStream::connect(socket_path).await.map_err(|e| {
            N2CClientError::ConnectionFailed(format!(
                "Cannot connect to {}: {e}",
                socket_path.display()
            ))
        })?;
        debug!("Connected to N2C socket: {}", socket_path.display());
        Ok(N2CClient { stream })
    }

    /// Perform the N2C handshake
    pub async fn handshake(&mut self, network_magic: u64) -> Result<(), N2CClientError> {
        // Build handshake proposal: [0, { version: [magic, false] }]
        // Propose versions 14-17 (N2C versions for recent eras)
        let mut proposal = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut proposal);
        enc.array(2)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(0)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?; // MsgProposeVersions
        enc.map(4)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;

        for version in [14u32, 15, 16, 17] {
            enc.u32(version)
                .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
            enc.array(2)
                .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
            enc.u64(network_magic)
                .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
            enc.bool(false)
                .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        }

        // Wrap in multiplexer segment
        let segment = Segment {
            transmission_time: 0,
            protocol_id: MINI_PROTOCOL_HANDSHAKE,
            is_responder: false,
            payload: proposal,
        };
        self.send_segment(&segment).await?;

        // Read response
        let resp = self.recv_segment().await?;
        if resp.protocol_id != MINI_PROTOCOL_HANDSHAKE {
            return Err(N2CClientError::Protocol(format!(
                "Expected handshake response, got protocol {}",
                resp.protocol_id
            )));
        }

        // Parse response: [1, version, params] = accept, [2, ...] = refuse
        let mut decoder = minicbor::Decoder::new(&resp.payload);
        let _ = decoder.array();
        let msg_tag = decoder
            .u32()
            .map_err(|e| N2CClientError::Protocol(format!("bad handshake response: {e}")))?;

        match msg_tag {
            1 => {
                let version = decoder.u32().unwrap_or(0);
                debug!("N2C handshake accepted, version {version}");
                Ok(())
            }
            2 => Err(N2CClientError::HandshakeRejected),
            _ => Err(N2CClientError::Protocol(format!(
                "unexpected handshake msg tag: {msg_tag}"
            ))),
        }
    }

    /// Acquire the ledger state at the current tip
    pub async fn acquire(&mut self) -> Result<(), N2CClientError> {
        // MsgAcquire: [0, point]
        // For "tip", we send [0, []] (acquire at tip)
        let mut payload = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut payload);
        enc.array(1)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(0)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?; // MsgAcquire

        let segment = Segment {
            transmission_time: 0,
            protocol_id: MINI_PROTOCOL_STATE_QUERY,
            is_responder: false,
            payload,
        };
        self.send_segment(&segment).await?;

        // Expect MsgAcquired [1]
        let resp = self.recv_segment().await?;
        let mut decoder = minicbor::Decoder::new(&resp.payload);
        let _ = decoder.array();
        let tag = decoder
            .u32()
            .map_err(|e| N2CClientError::Protocol(format!("bad acquire response: {e}")))?;
        if tag != 1 {
            return Err(N2CClientError::Protocol(format!(
                "expected MsgAcquired(1), got {tag}"
            )));
        }
        debug!("State acquired");
        Ok(())
    }

    /// Release the acquired state
    pub async fn release(&mut self) -> Result<(), N2CClientError> {
        let mut payload = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut payload);
        enc.array(1)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(7)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?; // MsgRelease

        let segment = Segment {
            transmission_time: 0,
            protocol_id: MINI_PROTOCOL_STATE_QUERY,
            is_responder: false,
            payload,
        };
        self.send_segment(&segment).await?;
        Ok(())
    }

    /// Send MsgDone to end the protocol
    pub async fn done(&mut self) -> Result<(), N2CClientError> {
        let mut payload = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut payload);
        enc.array(1)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(9)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?; // MsgDone

        let segment = Segment {
            transmission_time: 0,
            protocol_id: MINI_PROTOCOL_STATE_QUERY,
            is_responder: false,
            payload,
        };
        self.send_segment(&segment).await?;
        Ok(())
    }

    /// Query the chain tip (GetChainPoint - Shelley query tag 11)
    pub async fn query_tip(&mut self) -> Result<TipResult, N2CClientError> {
        let result = self.send_query(11).await?;
        parse_tip_result(&result)
    }

    /// Query the current epoch number (GetEpochNo - Shelley query tag 0)
    pub async fn query_epoch(&mut self) -> Result<u64, N2CClientError> {
        let result = self.send_query(0).await?;
        parse_epoch_result(&result)
    }

    /// Query the current era (GetCurrentEra - hardcoded query tag 0)
    pub async fn query_era(&mut self) -> Result<u32, N2CClientError> {
        // GetCurrentEra is a top-level query, not era-wrapped
        let mut payload = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut payload);
        enc.array(2)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(3)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?; // MsgQuery
        enc.array(1)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(0)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?; // GetCurrentEra

        let segment = Segment {
            transmission_time: 0,
            protocol_id: MINI_PROTOCOL_STATE_QUERY,
            is_responder: false,
            payload,
        };
        self.send_segment(&segment).await?;

        let resp = self.recv_segment().await?;
        let mut decoder = minicbor::Decoder::new(&resp.payload);
        let _ = decoder.array();
        let tag = decoder.u32().unwrap_or(999);
        if tag != 4 {
            return Err(N2CClientError::Protocol(format!(
                "expected MsgResult(4), got {tag}"
            )));
        }
        let era = decoder
            .u32()
            .map_err(|e| N2CClientError::Protocol(format!("bad era result: {e}")))?;
        Ok(era)
    }

    /// Query the chain block number (GetChainBlockNo - Shelley query tag 10)
    pub async fn query_block_no(&mut self) -> Result<u64, N2CClientError> {
        let result = self.send_query(10).await?;
        parse_u64_result(&result)
    }

    /// Send a Shelley-era query and return the raw MsgResult payload
    async fn send_query(&mut self, shelley_tag: u32) -> Result<Vec<u8>, N2CClientError> {
        // Build MsgQuery: [3, [era, [shelley_tag]]]
        let mut payload = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut payload);
        enc.array(2)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(3)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?; // MsgQuery
        enc.array(2)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(0)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?; // era tag (Shelley+)
        enc.array(1)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;
        enc.u32(shelley_tag)
            .map_err(|e| N2CClientError::Protocol(e.to_string()))?;

        let segment = Segment {
            transmission_time: 0,
            protocol_id: MINI_PROTOCOL_STATE_QUERY,
            is_responder: false,
            payload,
        };
        self.send_segment(&segment).await?;

        let resp = self.recv_segment().await?;
        Ok(resp.payload)
    }

    /// Send a multiplexer segment
    async fn send_segment(&mut self, segment: &Segment) -> Result<(), N2CClientError> {
        let encoded = segment.encode();
        self.stream.write_all(&encoded).await?;
        Ok(())
    }

    /// Receive a multiplexer segment
    async fn recv_segment(&mut self) -> Result<Segment, N2CClientError> {
        let mut buf = vec![0u8; 65536];
        let mut offset = 0;

        loop {
            let n = self.stream.read(&mut buf[offset..]).await?;
            if n == 0 {
                return Err(N2CClientError::Protocol("connection closed".into()));
            }
            offset += n;

            // Try to decode a segment
            match Segment::decode(&buf[..offset]) {
                Ok((segment, _consumed)) => {
                    return Ok(segment);
                }
                Err(_) => {
                    if offset >= buf.len() {
                        return Err(N2CClientError::Protocol("response too large".into()));
                    }
                    continue; // Need more data
                }
            }
        }
    }
}

/// Parse a tip result from MsgResult CBOR
fn parse_tip_result(payload: &[u8]) -> Result<TipResult, N2CClientError> {
    let mut decoder = minicbor::Decoder::new(payload);
    let _ = decoder.array();
    let tag = decoder.u32().unwrap_or(999);
    if tag != 4 {
        return Err(N2CClientError::Protocol(format!(
            "expected MsgResult(4), got {tag}"
        )));
    }

    // Result is: [[ slot, hash ], block_no]
    let _ = decoder.array();
    let _ = decoder.array();
    let slot = decoder
        .u64()
        .map_err(|e| N2CClientError::Protocol(format!("bad slot: {e}")))?;
    let hash = decoder
        .bytes()
        .map_err(|e| N2CClientError::Protocol(format!("bad hash: {e}")))?
        .to_vec();
    let block_no = decoder
        .u64()
        .map_err(|e| N2CClientError::Protocol(format!("bad block_no: {e}")))?;

    Ok(TipResult {
        slot,
        hash,
        block_no,
        epoch: 0, // Will be filled in by caller
        era: 0,
    })
}

/// Parse an epoch number from MsgResult CBOR
fn parse_epoch_result(payload: &[u8]) -> Result<u64, N2CClientError> {
    let mut decoder = minicbor::Decoder::new(payload);
    let _ = decoder.array();
    let tag = decoder.u32().unwrap_or(999);
    if tag != 4 {
        return Err(N2CClientError::Protocol(format!(
            "expected MsgResult(4), got {tag}"
        )));
    }
    decoder
        .u64()
        .map_err(|e| N2CClientError::Protocol(format!("bad epoch: {e}")))
}

/// Parse a u64 from MsgResult CBOR
fn parse_u64_result(payload: &[u8]) -> Result<u64, N2CClientError> {
    let mut decoder = minicbor::Decoder::new(payload);
    let _ = decoder.array();
    let tag = decoder.u32().unwrap_or(999);
    if tag != 4 {
        return Err(N2CClientError::Protocol(format!(
            "expected MsgResult(4), got {tag}"
        )));
    }
    decoder
        .u64()
        .map_err(|e| N2CClientError::Protocol(format!("bad u64: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tip_result() {
        // Build a MsgResult: [4, [[slot, hash], block_no]]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(4).unwrap();
        enc.array(2).unwrap();
        enc.array(2).unwrap();
        enc.u64(12345).unwrap();
        enc.bytes(&[0xab; 32]).unwrap();
        enc.u64(100).unwrap();

        let result = parse_tip_result(&buf).unwrap();
        assert_eq!(result.slot, 12345);
        assert_eq!(result.hash, vec![0xab; 32]);
        assert_eq!(result.block_no, 100);
    }

    #[test]
    fn test_parse_epoch_result() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(4).unwrap();
        enc.u64(500).unwrap();

        assert_eq!(parse_epoch_result(&buf).unwrap(), 500);
    }

    #[test]
    fn test_parse_u64_result() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(4).unwrap();
        enc.u64(42000).unwrap();

        assert_eq!(parse_u64_result(&buf).unwrap(), 42000);
    }

    #[test]
    fn test_parse_bad_tag_rejected() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(5).unwrap(); // Wrong tag
        enc.u64(100).unwrap();

        assert!(parse_u64_result(&buf).is_err());
    }
}
