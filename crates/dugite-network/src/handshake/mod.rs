//! Ouroboros handshake protocol implementation.
//!
//! The handshake is the first mini-protocol exchanged on a new connection (protocol ID 0).
//! It negotiates the protocol version and version data before any other communication.
//!
//! ## Client flow
//! 1. Send `MsgProposeVersions` with our supported versions + version data
//! 2. Receive one of:
//!    - `MsgAcceptVersion(version, version_data)` — negotiation succeeded
//!    - `MsgRefuse(version, reason)` — rejected
//!    - `MsgQueryReply(versions)` — if remote was in query mode
//!    - `MsgProposeVersions` — simultaneous open detected
//!
//! ## Server flow
//! 1. Receive `MsgProposeVersions` with remote's supported versions
//! 2. Select highest common version, verify magic, send `MsgAcceptVersion` or `MsgRefuse`
//!
//! ## Wire format (CBOR)
//! - `MsgProposeVersions` = `[0, {version: version_data, ...}]`
//! - `MsgAcceptVersion` = `[1, version, version_data]`
//! - `MsgRefuse` = `[2, [version_mismatch_tag, ...]]`
//! - `MsgQueryReply` = `[3, {version: version_data, ...}]`

pub mod n2c;
pub mod n2n;

use minicbor::{Decoder, Encoder};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::error::HandshakeError;
use crate::mux::channel::MuxChannel;

/// Handshake timeout — matches Haskell's 10-second handshake deadline.
/// Prevents indefinite blocking when a peer never responds.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub use n2c::N2CVersionData;
pub use n2n::N2NVersionData;

/// Result of a successful handshake.
#[derive(Debug, Clone)]
pub struct HandshakeResult {
    /// Negotiated protocol version.
    pub version: u16,
    /// Whether simultaneous open was detected (received MsgProposeVersions instead of MsgAccept).
    pub simultaneous_open: bool,
    /// Whether this was a query-mode handshake (#880): the peer opened with the
    /// `query` flag set, so we replied with `MsgQueryReply` (the version table)
    /// instead of `MsgAcceptVersion` and the connection carries no
    /// mini-protocols — the caller should close it after the reply. On the
    /// client side this is set when a `MsgQueryReply` was received.
    pub query: bool,
}

// ─── CBOR Message Tags ───

/// MsgProposeVersions tag (client → server).
const MSG_PROPOSE_VERSIONS: u64 = 0;
/// MsgAcceptVersion tag (server → client).
const MSG_ACCEPT_VERSION: u64 = 1;
/// MsgRefuse tag (server → client).
const MSG_REFUSE: u64 = 2;
/// MsgQueryReply tag (server → client, query mode only).
const MSG_QUERY_REPLY: u64 = 3;

/// Maximum number of version entries accepted in a `MsgProposeVersions` map.
///
/// A peer can send a CBOR map header advertising `2^63` entries, causing the
/// decode loop to run for billions of iterations, pegging a CPU core for the
/// 10-second handshake window. Haskell's `decodeVersions` is bounded to the
/// version table size (≤ 16 entries for N2N). We cap at 32 which is above
/// any real cardano-node version table while rejecting clearly adversarial
/// inputs early (A-003, security audit 2026-05-19).
const MAX_HANDSHAKE_VERSIONS: u64 = 32;

/// Run the handshake as the client (initiator) for N2N connections.
///
/// Sends `MsgProposeVersions` with our version table, then waits for the server's response.
/// Returns the negotiated version and whether simultaneous open was detected.
pub async fn run_n2n_handshake_client(
    channel: &mut MuxChannel,
    our_data: &N2NVersionData,
) -> Result<HandshakeResult, HandshakeError> {
    // Build and send MsgProposeVersions
    let msg = encode_propose_versions_n2n(n2n::N2N_VERSIONS, our_data);
    channel.send(msg).await.map_err(HandshakeError::from)?;

    // Receive response (with timeout to prevent indefinite blocking)
    let response = tokio::time::timeout(HANDSHAKE_TIMEOUT, channel.recv())
        .await
        .map_err(|_| HandshakeError::Timeout)?
        .map_err(HandshakeError::from)?;

    let result = decode_handshake_response(&response, our_data)?;

    // On simultaneous open, we negotiated from the remote's proposal — send
    // MsgAcceptVersion back so the remote also completes its handshake.
    if result.simultaneous_open {
        let accept = encode_accept_version_n2n(result.version, our_data);
        channel.send(accept).await.map_err(HandshakeError::from)?;
    }

    Ok(result)
}

/// Run the handshake as the server (responder) for N2N connections.
///
/// Receives `MsgProposeVersions`, selects the highest common version, validates
/// magic, and sends `MsgAcceptVersion` or `MsgRefuse`.
pub async fn run_n2n_handshake_server(
    channel: &mut MuxChannel,
    our_data: &N2NVersionData,
) -> Result<HandshakeResult, HandshakeError> {
    // Receive MsgProposeVersions (with timeout)
    let proposal = tokio::time::timeout(HANDSHAKE_TIMEOUT, channel.recv())
        .await
        .map_err(|_| HandshakeError::Timeout)?
        .map_err(HandshakeError::from)?;

    let remote_versions = decode_propose_versions_n2n(&proposal)?;

    // Find highest common version
    for &our_version in n2n::N2N_VERSIONS {
        if let Some(their_data) = remote_versions.get(&our_version) {
            // Check if we can accept this version
            if let Some(accepted) = our_data.accept(their_data) {
                // #880: if the peer opened in query mode, reply with
                // MsgQueryReply (our version table, tag 3) instead of
                // MsgAcceptVersion and flag the result as `query` so the caller
                // closes the connection. A query-mode client (e.g. a version
                // enumerator) has no tag-1 arm and would otherwise fail to
                // decode our accept. The Haskell handshake OR's the query flag,
                // so a query proposal from the peer puts us in query mode.
                if their_data.query {
                    let msg = encode_query_reply_n2n(n2n::N2N_VERSIONS, our_data);
                    channel.send(msg).await.map_err(HandshakeError::from)?;
                    return Ok(HandshakeResult {
                        version: our_version,
                        simultaneous_open: false,
                        query: true,
                    });
                }
                // #1104: MsgAcceptVersion must carry the NEGOTIATED
                // (accepted) version_data, not our own raw local data.
                // Haskell's `acceptableVersion` (Cardano.Network.NodeToNode.Version)
                // returns `Accept NodeToNodeVersionData { diffusionMode =
                // diffusionMode local `min` diffusionMode remote, peerSharing =
                // peerSharing local <> peerSharing remote, ... }` — a per-field
                // min/AND/OR reduction of local and remote, NOT a copy of
                // either side's raw proposal — and the Handshake protocol
                // driver (Ouroboros.Network.Protocol.Handshake.Server) sends
                // exactly that `Accept`-computed record in `MsgAcceptVersion`.
                // Sending `our_data` unmodified told a peer e.g. peer_sharing
                // was enabled when the negotiated (AND'd) value was disabled.
                let msg = encode_accept_version_n2n(our_version, &accepted);
                channel.send(msg).await.map_err(HandshakeError::from)?;
                return Ok(HandshakeResult {
                    version: our_version,
                    simultaneous_open: false,
                    query: false,
                });
            } else {
                // Magic mismatch — use Refused (tag 2) with the matched version and a reason string
                let msg = encode_refuse_with_reason(2, our_version, "network magic mismatch");
                channel.send(msg).await.map_err(HandshakeError::from)?;
                return Err(HandshakeError::NetworkMagicMismatch {
                    ours: our_data.network_magic,
                    theirs: their_data.network_magic,
                });
            }
        }
    }

    // No common version — emit VersionMismatch with our supported version list
    let our_versions: Vec<u16> = n2n::N2N_VERSIONS.to_vec();
    let their_versions: Vec<u16> = remote_versions.keys().copied().collect();
    let msg = encode_refuse_version_mismatch(&our_versions);
    let _ = channel.send(msg).await;
    Err(HandshakeError::VersionMismatch {
        ours: our_versions,
        theirs: their_versions,
    })
}

/// Run the handshake as the client (initiator) for N2C connections.
pub async fn run_n2c_handshake_client(
    channel: &mut MuxChannel,
    our_data: &N2CVersionData,
) -> Result<HandshakeResult, HandshakeError> {
    let msg = encode_propose_versions_n2c(n2c::N2C_VERSIONS, our_data);
    channel.send(msg).await.map_err(HandshakeError::from)?;

    // Receive response (with timeout)
    let response = tokio::time::timeout(HANDSHAKE_TIMEOUT, channel.recv())
        .await
        .map_err(|_| HandshakeError::Timeout)?
        .map_err(HandshakeError::from)?;

    // Decode, converting wire versions back to logical
    decode_handshake_response_n2c(&response, our_data)
}

/// Run the handshake as the server (responder) for N2C connections.
pub async fn run_n2c_handshake_server(
    channel: &mut MuxChannel,
    our_data: &N2CVersionData,
) -> Result<HandshakeResult, HandshakeError> {
    // Receive MsgProposeVersions (with timeout)
    let proposal = tokio::time::timeout(HANDSHAKE_TIMEOUT, channel.recv())
        .await
        .map_err(|_| HandshakeError::Timeout)?
        .map_err(HandshakeError::from)?;

    let remote_versions = decode_propose_versions_n2c(&proposal)?;

    // Find highest common version (N2C versions are already logical after decode)
    for &our_version in n2c::N2C_VERSIONS {
        if let Some(their_data) = remote_versions.get(&our_version) {
            if let Some(accepted) = our_data.accept(their_data) {
                // #1101: if the peer opened in query mode, reply with
                // MsgQueryReply (our version table, tag 3) instead of
                // MsgAcceptVersion and flag the result as `query` so the
                // caller closes the connection — mirrors
                // `run_n2n_handshake_server`'s #880 query-mode branch, which
                // this function had never implemented.
                if their_data.query {
                    let msg = encode_query_reply_n2c(n2c::N2C_VERSIONS, our_data);
                    channel.send(msg).await.map_err(HandshakeError::from)?;
                    return Ok(HandshakeResult {
                        version: our_version,
                        simultaneous_open: false,
                        query: true,
                    });
                }
                // #1104: send the NEGOTIATED (accepted) version_data, not our
                // raw local data — see the identical fix + Haskell grounding
                // in `run_n2n_handshake_server` above. For N2CVersionData's
                // current two fields this happens to be byte-identical to
                // `our_data` in every case reachable here (network_magic must
                // already match to reach `Some`, and `query` can only differ
                // in the direction that diverts into the MsgQueryReply arm
                // above), but using the accepted value is still the correct
                // and robust thing to send — it stops being an accident the
                // moment N2CVersionData grows a field with real min/AND/OR
                // semantics, the way N2NVersionData already has.
                let msg = encode_accept_version_n2c(our_version, &accepted);
                channel.send(msg).await.map_err(HandshakeError::from)?;
                return Ok(HandshakeResult {
                    version: our_version,
                    simultaneous_open: false,
                    query: false,
                });
            } else {
                // Magic mismatch — use Refused (tag 2); wire-encode the version number
                let msg = encode_refuse_with_reason(
                    2,
                    n2c::encode_n2c_version(our_version),
                    "network magic mismatch",
                );
                channel.send(msg).await.map_err(HandshakeError::from)?;
                return Err(HandshakeError::NetworkMagicMismatch {
                    ours: our_data.network_magic,
                    theirs: their_data.network_magic,
                });
            }
        }
    }

    // No common version — emit VersionMismatch with our supported version list (wire-encoded)
    let our_versions: Vec<u16> = n2c::N2C_VERSIONS.to_vec();
    let their_versions: Vec<u16> = remote_versions.keys().copied().collect();
    let wire_versions: Vec<u16> = our_versions
        .iter()
        .map(|&v| n2c::encode_n2c_version(v))
        .collect();
    let msg = encode_refuse_version_mismatch(&wire_versions);
    let _ = channel.send(msg).await;
    Err(HandshakeError::VersionMismatch {
        ours: our_versions,
        theirs: their_versions,
    })
}

/// Query the peer's full supported-version table over N2N
/// (`dugite-cli ping -Q`/`--query-versions`, #1091).
///
/// Distinct from `run_n2n_handshake_client`'s `HandshakeResult`, which
/// collapses a query reply into a single best-match version and DISCARDS
/// each entry's `N2NVersionData` (`decode_handshake_response`'s
/// `MSG_QUERY_REPLY` arm — correct for #880's negotiation-diagnostic use,
/// insufficient for listing every version with its own initiator/peer-
/// sharing flags, which is what `ping -Q` needs to report).
///
/// Handles two reply shapes, both verified against real peers: a
/// `MsgQueryReply` (tag 3) carrying the full version table — what a real
/// cardano-node's N2C/N2N server sends — and, for interop with a server
/// that does not implement query mode at all, a plain `MsgAcceptVersion`
/// (tag 1) is treated as a one-entry table (the single version it
/// accepted). A real `cardano-cli ping -Q` against dugite-node's own N2C
/// socket was observed doing exactly this fallback.
pub async fn query_n2n_versions(
    channel: &mut MuxChannel,
    network_magic: u64,
) -> Result<Vec<(u16, N2NVersionData)>, HandshakeError> {
    let mut our_data = N2NVersionData::new(network_magic, true, false);
    our_data.query = true;
    let msg = encode_propose_versions_n2n(n2n::N2N_VERSIONS, &our_data);
    channel.send(msg).await.map_err(HandshakeError::from)?;

    let response = tokio::time::timeout(HANDSHAKE_TIMEOUT, channel.recv())
        .await
        .map_err(|_| HandshakeError::Timeout)?
        .map_err(HandshakeError::from)?;

    decode_query_reply_n2n(&response)
}

/// As [`query_n2n_versions`], for N2C.
pub async fn query_n2c_versions(
    channel: &mut MuxChannel,
    network_magic: u64,
) -> Result<Vec<(u16, N2CVersionData)>, HandshakeError> {
    let mut our_data = N2CVersionData::new(network_magic);
    our_data.query = true;
    let msg = encode_propose_versions_n2c(n2c::N2C_VERSIONS, &our_data);
    channel.send(msg).await.map_err(HandshakeError::from)?;

    let response = tokio::time::timeout(HANDSHAKE_TIMEOUT, channel.recv())
        .await
        .map_err(|_| HandshakeError::Timeout)?
        .map_err(HandshakeError::from)?;

    decode_query_reply_n2c(&response)
}

fn decode_query_reply_n2n(data: &[u8]) -> Result<Vec<(u16, N2NVersionData)>, HandshakeError> {
    let mut dec = Decoder::new(data);
    let _ = dec
        .array()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
    let tag = dec
        .u64()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;

    if tag == MSG_ACCEPT_VERSION {
        // Server does not implement query mode; it just accepted the best
        // common version. Report that single version — see doc comment.
        let version = dec
            .u16()
            .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        let data = N2NVersionData::decode(&mut dec)
            .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        return Ok(vec![(version, data)]);
    }
    if tag == MSG_REFUSE {
        return Err(decode_refuse_reason(&mut dec));
    }
    if tag != MSG_QUERY_REPLY {
        return Err(HandshakeError::DecodeError(format!(
            "expected MsgQueryReply (tag 3), got {tag}"
        )));
    }

    let map_len = dec
        .map()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?
        .ok_or_else(|| HandshakeError::DecodeError("indefinite map not supported".to_string()))?;
    if map_len > MAX_HANDSHAKE_VERSIONS {
        return Err(HandshakeError::DecodeError(format!(
            "MsgQueryReply: too many versions ({map_len} > {MAX_HANDSHAKE_VERSIONS})"
        )));
    }
    let mut versions = Vec::new();
    for _ in 0..map_len {
        let version = dec
            .u16()
            .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        if n2n::N2N_VERSIONS.contains(&version) {
            let data = N2NVersionData::decode(&mut dec)
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
            versions.push((version, data));
        } else {
            dec.skip()
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        }
    }
    Ok(versions)
}

fn decode_query_reply_n2c(data: &[u8]) -> Result<Vec<(u16, N2CVersionData)>, HandshakeError> {
    let mut dec = Decoder::new(data);
    let _ = dec
        .array()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
    let tag = dec
        .u64()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;

    if tag == MSG_ACCEPT_VERSION {
        let wire_version = dec
            .u16()
            .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        let version = n2c::decode_n2c_version(wire_version);
        let data = N2CVersionData::decode(&mut dec)
            .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        return Ok(vec![(version, data)]);
    }
    if tag == MSG_REFUSE {
        return Err(decode_refuse_reason(&mut dec));
    }
    if tag != MSG_QUERY_REPLY {
        return Err(HandshakeError::DecodeError(format!(
            "expected MsgQueryReply (tag 3), got {tag}"
        )));
    }

    let map_len = dec
        .map()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?
        .ok_or_else(|| HandshakeError::DecodeError("indefinite map not supported".to_string()))?;
    if map_len > MAX_HANDSHAKE_VERSIONS {
        return Err(HandshakeError::DecodeError(format!(
            "MsgQueryReply: too many versions ({map_len} > {MAX_HANDSHAKE_VERSIONS})"
        )));
    }
    let mut versions = Vec::new();
    for _ in 0..map_len {
        let wire_version = dec
            .u16()
            .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        let logical_version = n2c::decode_n2c_version(wire_version);
        if n2c::N2C_VERSIONS.contains(&logical_version) {
            let data = N2CVersionData::decode(&mut dec)
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
            versions.push((logical_version, data));
        } else {
            dec.skip()
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        }
    }
    Ok(versions)
}

// ─── Encoding helpers ───

/// Encode MsgProposeVersions for N2N: `[0, {version: version_data, ...}]`.
///
/// Map keys MUST be sorted in ascending order — the Haskell node requires
/// canonical CBOR encoding (RFC 7049 §3.9) for the handshake map.
fn encode_propose_versions_n2n(versions: &[u16], data: &N2NVersionData) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).expect("infallible");
    enc.u64(MSG_PROPOSE_VERSIONS).expect("infallible");
    // Sort versions ascending for canonical CBOR map key ordering
    let mut sorted_versions: Vec<u16> = versions.to_vec();
    sorted_versions.sort();
    enc.map(sorted_versions.len() as u64).expect("infallible");
    for v in &sorted_versions {
        enc.u16(*v).expect("infallible");
        data.encode(&mut enc);
    }
    buf
}

/// Encode MsgProposeVersions for N2C with bit-15 wire encoding.
///
/// Map keys MUST be sorted ascending (canonical CBOR).
fn encode_propose_versions_n2c(versions: &[u16], data: &N2CVersionData) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).expect("infallible");
    enc.u64(MSG_PROPOSE_VERSIONS).expect("infallible");
    let mut sorted_versions: Vec<u16> = versions.to_vec();
    sorted_versions.sort();
    enc.map(sorted_versions.len() as u64).expect("infallible");
    for v in &sorted_versions {
        enc.u16(n2c::encode_n2c_version(*v)).expect("infallible");
        data.encode(&mut enc);
    }
    buf
}

/// Encode MsgQueryReply for N2N: `[3, {version: version_data, ...}]` (#880).
///
/// Sent by the responder when the initiator opened the handshake in query mode:
/// it carries our full version table (same map shape as MsgProposeVersions) so
/// the querying tool can enumerate supported versions, then the connection is
/// closed. Map keys MUST be sorted ascending (canonical CBOR).
fn encode_query_reply_n2n(versions: &[u16], data: &N2NVersionData) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).expect("infallible");
    enc.u64(MSG_QUERY_REPLY).expect("infallible");
    let mut sorted_versions: Vec<u16> = versions.to_vec();
    sorted_versions.sort();
    enc.map(sorted_versions.len() as u64).expect("infallible");
    for v in &sorted_versions {
        enc.u16(*v).expect("infallible");
        data.encode(&mut enc);
    }
    buf
}

/// Encode MsgQueryReply for N2C: `[3, {version: version_data, ...}]` (#1101).
///
/// Sent by the responder when the initiator opened the N2C handshake in query
/// mode: mirrors `encode_query_reply_n2n`, carrying our full version table so
/// the querying tool can enumerate supported versions, then the connection is
/// closed. Versions are wire-encoded with bit-15 set, same as
/// `encode_propose_versions_n2c`. Map keys MUST be sorted ascending
/// (canonical CBOR).
fn encode_query_reply_n2c(versions: &[u16], data: &N2CVersionData) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).expect("infallible");
    enc.u64(MSG_QUERY_REPLY).expect("infallible");
    let mut sorted_versions: Vec<u16> = versions.to_vec();
    sorted_versions.sort();
    enc.map(sorted_versions.len() as u64).expect("infallible");
    for v in &sorted_versions {
        enc.u16(n2c::encode_n2c_version(*v)).expect("infallible");
        data.encode(&mut enc);
    }
    buf
}

/// Encode MsgAcceptVersion for N2N: `[1, version, version_data]`.
fn encode_accept_version_n2n(version: u16, data: &N2NVersionData) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(3).expect("infallible");
    enc.u64(MSG_ACCEPT_VERSION).expect("infallible");
    enc.u16(version).expect("infallible");
    data.encode(&mut enc);
    buf
}

/// Encode MsgAcceptVersion for N2C with bit-15 wire encoding.
fn encode_accept_version_n2c(version: u16, data: &N2CVersionData) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(3).expect("infallible");
    enc.u64(MSG_ACCEPT_VERSION).expect("infallible");
    enc.u16(n2c::encode_n2c_version(version))
        .expect("infallible");
    data.encode(&mut enc);
    buf
}

/// Encode MsgRefuse for a VersionMismatch: `[2, [0, [v1, v2, ...]]]`.
///
/// Per CDDL `refuseReasonVersionMismatch = (0, [*versionNumber])`.
/// The second element is a list of the versions *we* support — not a
/// `(version, reason_text)` pair as was previously (incorrectly) encoded.
fn encode_refuse_version_mismatch(our_versions: &[u16]) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).expect("infallible");
    enc.u64(MSG_REFUSE).expect("infallible");
    // RefuseReason: [0, [v1, v2, ...]] — tag 0 with our supported version list
    enc.array(2).expect("infallible");
    enc.u8(0).expect("infallible");
    enc.array(our_versions.len() as u64).expect("infallible");
    for v in our_versions {
        enc.u16(*v).expect("infallible");
    }
    buf
}

/// Encode MsgRefuse for a non-version-mismatch reason: `[2, [tag, version, reason_text]]`.
///
/// Used for HandshakeDecodeError (tag 1) and Refused (tag 2), both of which carry
/// `(tag, versionNumber, text)` per the CDDL spec.
fn encode_refuse_with_reason(tag: u8, version: u16, reason: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut enc = Encoder::new(&mut buf);
    enc.array(2).expect("infallible");
    enc.u64(MSG_REFUSE).expect("infallible");
    // RefuseReason: [tag, version, reason_text]
    enc.array(3).expect("infallible");
    enc.u8(tag).expect("infallible");
    enc.u16(version).expect("infallible");
    enc.str(reason).expect("infallible");
    buf
}

// ─── Decoding helpers ───

/// Decode MsgProposeVersions for N2N. Returns a map of version → version_data.
fn decode_propose_versions_n2n(
    data: &[u8],
) -> Result<BTreeMap<u16, N2NVersionData>, HandshakeError> {
    let mut dec = Decoder::new(data);
    let _arr_len = dec
        .array()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
    let tag = dec
        .u64()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
    if tag != MSG_PROPOSE_VERSIONS {
        return Err(HandshakeError::DecodeError(format!(
            "expected MsgProposeVersions (tag 0), got {tag}"
        )));
    }

    let map_len = dec
        .map()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?
        .ok_or_else(|| HandshakeError::DecodeError("indefinite map not supported".to_string()))?;

    // A-003 (security audit 2026-05-19): reject absurdly large version maps
    // before iterating. A malicious peer advertising 2^63 entries would peg a
    // CPU core for the full 10-second handshake window. Cap at 32 — above any
    // real cardano-node version table (≤ 16 for N2N).
    if map_len > MAX_HANDSHAKE_VERSIONS {
        return Err(HandshakeError::DecodeError(format!(
            "MsgProposeVersions: too many versions ({map_len} > {MAX_HANDSHAKE_VERSIONS})"
        )));
    }

    let mut versions = BTreeMap::new();
    for _ in 0..map_len {
        let version = dec
            .u16()
            .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        // Cardano-node sends ALL the versions it supports in the proposal
        // map, including older ones like N2N v13 whose `version_data` has a
        // different CBOR shape (array(3) instead of array(4) — no `query`).
        // Decode only versions we know how to negotiate; skip the rest by
        // consuming whatever CBOR item follows the version key.  This mirrors
        // the Haskell handshake which uses `acceptableVersion` after the
        // proposal is fully decoded — unknown versions are filtered, not
        // rejected.
        if n2n::N2N_VERSIONS.contains(&version) {
            let version_data = N2NVersionData::decode(&mut dec)
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
            versions.insert(version, version_data);
        } else {
            dec.skip()
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        }
    }
    Ok(versions)
}

/// Decode MsgProposeVersions for N2C. Converts wire versions (bit-15) to logical.
fn decode_propose_versions_n2c(
    data: &[u8],
) -> Result<BTreeMap<u16, N2CVersionData>, HandshakeError> {
    let mut dec = Decoder::new(data);
    let _arr_len = dec
        .array()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
    let tag = dec
        .u64()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
    if tag != MSG_PROPOSE_VERSIONS {
        return Err(HandshakeError::DecodeError(format!(
            "expected MsgProposeVersions (tag 0), got {tag}"
        )));
    }

    let map_len = dec
        .map()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?
        .ok_or_else(|| HandshakeError::DecodeError("indefinite map not supported".to_string()))?;

    // A-003 (security audit 2026-05-19): same cap as the N2N path.
    if map_len > MAX_HANDSHAKE_VERSIONS {
        return Err(HandshakeError::DecodeError(format!(
            "MsgProposeVersions: too many versions ({map_len} > {MAX_HANDSHAKE_VERSIONS})"
        )));
    }

    let mut versions = BTreeMap::new();
    for _ in 0..map_len {
        let wire_version = dec
            .u16()
            .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        let logical_version = n2c::decode_n2c_version(wire_version);
        // Skip versions we don't support so a peer offering an older
        // version_data shape doesn't break the decode of versions we DO
        // support. Mirrors the n2n decoder behaviour.
        if n2c::N2C_VERSIONS.contains(&logical_version) {
            let version_data = N2CVersionData::decode(&mut dec)
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
            versions.insert(logical_version, version_data);
        } else {
            dec.skip()
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
        }
    }
    Ok(versions)
}

/// Decode a RefuseReason from a CBOR decoder positioned after the `[2, ...]` tag.
///
/// RefuseReason variants per CDDL:
/// - `[0, [v1, v2, ...]]` — VersionMismatch
/// - `[1, version, reason_text]` — HandshakeDecodeError
/// - `[2, version, reason_text]` — Refused
fn decode_refuse_reason(dec: &mut Decoder<'_>) -> HandshakeError {
    let _reason_arr = dec.array().ok();
    let reason_tag = dec.u8().unwrap_or(255);
    let reason = match reason_tag {
        0 => {
            // VersionMismatch: [0, [v1, v2, ...]] per CDDL refuseReasonVersionMismatch.
            //
            // #554: Cap the array length at MAX_HANDSHAKE_VERSIONS BEFORE
            // iterating. Without this cap a peer could declare
            // `array(u64::MAX)` and force `(0..n).collect()` to spin for
            // ~9e18 iterations (and the implicit `Vec::with_capacity` in
            // `collect()` for `n=u64::MAX as usize` would attempt a huge
            // allocation on 64-bit hosts).
            let versions: Vec<u16> = if let Ok(Some(n)) = dec.array() {
                let n = n.min(MAX_HANDSHAKE_VERSIONS);
                (0..n).filter_map(|_| dec.u16().ok()).collect()
            } else {
                vec![]
            };
            format!("version mismatch; remote supports: {versions:?}")
        }
        1 => {
            // HandshakeDecodeError: [1, version, reason_text]
            let v = dec.u16().unwrap_or(0);
            let r = dec.str().unwrap_or("unknown").to_owned();
            format!("handshake decode error (v{v}): {r}")
        }
        2 => {
            // Refused: [2, version, reason_text]
            let v = dec.u16().unwrap_or(0);
            let r = dec.str().unwrap_or("unknown").to_owned();
            format!("refused (v{v}): {r}")
        }
        _ => format!("unknown refuse reason tag {reason_tag}"),
    };
    HandshakeError::Refused { version: 0, reason }
}

/// Decode a handshake response (MsgAcceptVersion, MsgRefuse, or MsgProposeVersions for N2N).
///
/// On simultaneous open (receiving MsgProposeVersions instead of MsgAcceptVersion), decodes
/// the remote's version map and negotiates the highest common version — effectively acting
/// as the responder. Both sides do this and converge on the same version since `N2N_VERSIONS`
/// preference order is identical.
fn decode_handshake_response(
    data: &[u8],
    our_data: &N2NVersionData,
) -> Result<HandshakeResult, HandshakeError> {
    let mut dec = Decoder::new(data);
    let _arr_len = dec
        .array()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
    let tag = dec
        .u64()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;

    match tag {
        MSG_ACCEPT_VERSION => {
            let version = dec
                .u16()
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
            // #880: re-validate the responder's network magic instead of
            // discarding the accepted version_data. A peer that accepts with a
            // mismatched magic is on a different network; reject at handshake
            // rather than relying on later block validation to catch it. The
            // accepted version is one we support, so its version_data has our
            // shape and decodes cleanly; tolerate a decode failure (older shape)
            // since magic was already asserted at propose time.
            if let Ok(their_data) = N2NVersionData::decode(&mut dec) {
                if their_data.network_magic != our_data.network_magic {
                    return Err(HandshakeError::NetworkMagicMismatch {
                        ours: our_data.network_magic,
                        theirs: their_data.network_magic,
                    });
                }
            }
            Ok(HandshakeResult {
                version,
                simultaneous_open: false,
                query: false,
            })
        }
        MSG_QUERY_REPLY => {
            // #880: the responder answered our query-mode proposal with its
            // version table. Return the highest version we both support (best
            // effort) flagged as a query result; the caller closes the
            // connection (no mini-protocols run in query mode).
            let map_len = dec
                .map()
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?
                .ok_or_else(|| {
                    HandshakeError::DecodeError("indefinite map not supported".to_string())
                })?;
            if map_len > MAX_HANDSHAKE_VERSIONS {
                return Err(HandshakeError::DecodeError(format!(
                    "MsgQueryReply: too many versions ({map_len} > {MAX_HANDSHAKE_VERSIONS})"
                )));
            }
            let mut their_versions = std::collections::BTreeSet::new();
            for _ in 0..map_len {
                let version = dec
                    .u16()
                    .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
                if n2n::N2N_VERSIONS.contains(&version) {
                    // Skip the version_data of a known version.
                    let _ = N2NVersionData::decode(&mut dec);
                } else {
                    dec.skip()
                        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
                }
                their_versions.insert(version);
            }
            let version = n2n::N2N_VERSIONS
                .iter()
                .find(|v| their_versions.contains(v))
                .copied()
                .unwrap_or(0);
            Ok(HandshakeResult {
                version,
                simultaneous_open: false,
                query: true,
            })
        }
        MSG_REFUSE => Err(decode_refuse_reason(&mut dec)),
        MSG_PROPOSE_VERSIONS => {
            // Simultaneous open — the remote also sent MsgProposeVersions.
            // Decode their version map and negotiate from it, acting as responder.
            let map_len = dec
                .map()
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?
                .ok_or_else(|| {
                    HandshakeError::DecodeError("indefinite map not supported".to_string())
                })?;

            // #880: cap the version count (parity with
            // decode_propose_versions_n2n) so a simultaneous open can't be used
            // to peg a CPU core past the handshake window.
            if map_len > MAX_HANDSHAKE_VERSIONS {
                return Err(HandshakeError::DecodeError(format!(
                    "MsgProposeVersions (simultaneous open): too many versions \
                     ({map_len} > {MAX_HANDSHAKE_VERSIONS})"
                )));
            }

            let mut remote_versions = BTreeMap::new();
            for _ in 0..map_len {
                let version = dec
                    .u16()
                    .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
                // #880: skip versions we don't know (cardano-node includes older
                // ones like N2N v13 whose version_data has a different CBOR shape,
                // array(3) not array(4)); decoding every one made a genuine
                // simultaneous open fail with DecodeError instead of negotiating.
                // Mirror decode_propose_versions_n2n's known-version filter.
                if n2n::N2N_VERSIONS.contains(&version) {
                    let version_data = N2NVersionData::decode(&mut dec)
                        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
                    remote_versions.insert(version, version_data);
                } else {
                    dec.skip()
                        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
                }
            }

            // Find highest common version (same logic as server-side negotiation)
            for &our_version in n2n::N2N_VERSIONS {
                if let Some(their_data) = remote_versions.get(&our_version) {
                    if our_data.accept(their_data).is_some() {
                        return Ok(HandshakeResult {
                            version: our_version,
                            simultaneous_open: true,
                            query: false,
                        });
                    }
                    // Magic mismatch on a matching version — reject
                    return Err(HandshakeError::NetworkMagicMismatch {
                        ours: our_data.network_magic,
                        theirs: their_data.network_magic,
                    });
                }
            }

            // No common version
            let our_versions: Vec<u16> = n2n::N2N_VERSIONS.to_vec();
            let their_versions: Vec<u16> = remote_versions.keys().copied().collect();
            Err(HandshakeError::VersionMismatch {
                ours: our_versions,
                theirs: their_versions,
            })
        }
        _ => Err(HandshakeError::DecodeError(format!(
            "unexpected handshake message tag: {tag}"
        ))),
    }
}

/// Decode a handshake response for N2C (with bit-15 version decoding).
fn decode_handshake_response_n2c(
    data: &[u8],
    our_data: &N2CVersionData,
) -> Result<HandshakeResult, HandshakeError> {
    let mut dec = Decoder::new(data);
    let _arr_len = dec
        .array()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
    let tag = dec
        .u64()
        .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;

    match tag {
        MSG_ACCEPT_VERSION => {
            let wire_version = dec
                .u16()
                .map_err(|e| HandshakeError::DecodeError(e.to_string()))?;
            let version = n2c::decode_n2c_version(wire_version);
            // #880: re-validate the responder's network magic rather than
            // discarding the accepted version_data.
            if let Ok(their_data) = N2CVersionData::decode(&mut dec) {
                if their_data.network_magic != our_data.network_magic {
                    return Err(HandshakeError::NetworkMagicMismatch {
                        ours: our_data.network_magic,
                        theirs: their_data.network_magic,
                    });
                }
            }
            Ok(HandshakeResult {
                version,
                simultaneous_open: false,
                query: false,
            })
        }
        MSG_REFUSE => Err(decode_refuse_reason(&mut dec)),
        _ => Err(HandshakeError::DecodeError(format!(
            "unexpected N2C handshake message tag: {tag}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PROTOCOL_HANDSHAKE;

    /// #880: query mode — the responder answers a query-mode proposal with
    /// MsgQueryReply (tag 3, the version table), and the client decodes it as a
    /// query result rather than choking on an unexpected tag.
    #[test]
    fn query_mode_reply_roundtrip() {
        let our_data = N2NVersionData::new(2, false, true);
        // Server-side: encode MsgQueryReply as we would emit it.
        let reply = encode_query_reply_n2n(n2n::N2N_VERSIONS, &our_data);
        // First byte structure: array(2), tag 3.
        let mut dec = Decoder::new(&reply);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u64().unwrap(), MSG_QUERY_REPLY);

        // Client-side: decode_handshake_response must classify it as a query.
        let result = decode_handshake_response(&reply, &our_data).unwrap();
        assert!(result.query, "MsgQueryReply must yield a query result");
        assert!(
            n2n::N2N_VERSIONS.contains(&result.version),
            "query result reports a common version"
        );
    }

    /// #880: a MsgAcceptVersion whose version_data carries a DIFFERENT network
    /// magic than ours must be rejected at handshake (cross-network peer),
    /// rather than the accepted version_data being silently discarded.
    #[test]
    fn accept_version_with_mismatched_magic_is_rejected() {
        let ours = N2NVersionData::new(2, false, true); // preview magic 2
        let theirs = N2NVersionData::new(764824073, false, true); // mainnet magic
        let accept = encode_accept_version_n2n(n2n::N2N_VERSIONS[0], &theirs);
        let result = decode_handshake_response(&accept, &ours);
        assert!(
            matches!(
                result,
                Err(HandshakeError::NetworkMagicMismatch {
                    ours: 2,
                    theirs: 764824073
                })
            ),
            "cross-network MsgAcceptVersion must be rejected, got: {result:?}"
        );

        // Sanity: a matching magic still accepts.
        let ok = encode_accept_version_n2n(n2n::N2N_VERSIONS[0], &ours);
        assert!(decode_handshake_response(&ok, &ours).is_ok());
    }

    #[test]
    fn n2n_propose_encode_decode_roundtrip() {
        let data = N2NVersionData::new(2, false, true);
        let encoded = encode_propose_versions_n2n(n2n::N2N_VERSIONS, &data);
        let decoded = decode_propose_versions_n2n(&encoded).unwrap();
        assert!(decoded.contains_key(&14));
        assert!(decoded.contains_key(&15));
        assert_eq!(decoded[&14].network_magic, 2);
        assert!(decoded[&15].peer_sharing);
    }

    #[test]
    fn n2n_accept_encode_decode() {
        let data = N2NVersionData::new(2, false, true);
        let encoded = encode_accept_version_n2n(15, &data);
        let result = decode_handshake_response(&encoded, &data).unwrap();
        assert_eq!(result.version, 15);
        assert!(!result.simultaneous_open);
    }

    #[test]
    fn n2n_refuse_version_mismatch_decode() {
        // VersionMismatch (tag 0): [0, [v1, v2, ...]] per CDDL
        let data = N2NVersionData::new(2, false, true);
        let encoded = encode_refuse_version_mismatch(&[14, 15]);
        let result = decode_handshake_response(&encoded, &data);
        assert!(result.is_err());
        if let Err(HandshakeError::Refused { reason, .. }) = result {
            assert!(
                reason.contains("version mismatch"),
                "expected 'version mismatch' in reason, got: {reason}"
            );
        } else {
            panic!("expected HandshakeError::Refused");
        }
    }

    #[test]
    fn n2n_refuse_refused_decode() {
        // Refused (tag 2): [2, version, reason_text]
        let data = N2NVersionData::new(2, false, true);
        let encoded = encode_refuse_with_reason(2, 15, "bad magic");
        let result = decode_handshake_response(&encoded, &data);
        assert!(result.is_err());
        if let Err(HandshakeError::Refused { reason, .. }) = result {
            assert!(reason.contains("bad magic"));
        } else {
            panic!("expected HandshakeError::Refused");
        }
    }

    #[test]
    fn n2c_propose_encode_decode_roundtrip() {
        let data = N2CVersionData::new(2);
        let encoded = encode_propose_versions_n2c(n2c::N2C_VERSIONS, &data);
        let decoded = decode_propose_versions_n2c(&encoded).unwrap();
        // All 8 N2C versions should be present (as logical versions)
        for &v in n2c::N2C_VERSIONS {
            assert!(decoded.contains_key(&v), "missing version {v}");
        }
        assert_eq!(decoded[&16].network_magic, 2);
    }

    #[test]
    fn n2c_accept_encode_decode() {
        let data = N2CVersionData::new(2);
        let encoded = encode_accept_version_n2c(22, &data);
        let result = decode_handshake_response_n2c(&encoded, &data).unwrap();
        assert_eq!(result.version, 22); // logical, not wire

        // #880: a mismatched magic on the accepted version_data is rejected.
        let ours = N2CVersionData::new(1);
        assert!(matches!(
            decode_handshake_response_n2c(&encoded, &ours),
            Err(HandshakeError::NetworkMagicMismatch { .. })
        ));
    }

    #[test]
    fn n2c_bit15_wire_format() {
        // Verify the wire format contains bit-15 encoded versions
        let data = N2CVersionData::new(2);
        let encoded = encode_propose_versions_n2c(&[n2c::N2C_V16], &data);
        // The encoded bytes should contain 32784 (V16 | 0x8000) as a CBOR integer
        let mut dec = Decoder::new(&encoded);
        dec.array().unwrap(); // outer array
        dec.u64().unwrap(); // tag 0
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(map_len, 1);
        let wire_version = dec.u16().unwrap();
        assert_eq!(wire_version, 32784); // 16 | 0x8000
    }

    /// #1101: `run_n2c_handshake_server` never checked the client's `query`
    /// flag and always replied with `MsgAcceptVersion`, unlike its N2N
    /// sibling `run_n2n_handshake_server` (which has the correct branch —
    /// see `query_mode_reply_roundtrip` above). Drives the REAL server
    /// function through a `MuxChannel` backed by plain mpsc queues (the same
    /// idiom used by every other protocol server test in this crate, e.g.
    /// `peersharing::server::tests::make_test_channel`), sends a
    /// `MsgProposeVersions` with `query = true`, and asserts the reply on the
    /// wire is `MsgQueryReply` (tag 3) carrying the full `N2C_VERSIONS`
    /// table — not a lone `MsgAcceptVersion` (tag 1).
    #[tokio::test]
    async fn n2c_query_mode_server_replies_with_query_reply() {
        use bytes::Bytes;
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;
        use tokio::sync::mpsc;

        let (egress_tx, mut egress_rx) = mpsc::channel(8);
        let (ingress_tx, ingress_rx) = mpsc::channel(8);
        let mut channel = MuxChannel::new(
            0,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            65536,
            Arc::new(AtomicUsize::new(0)),
        );

        let our_data = N2CVersionData::new(2);
        let handle =
            tokio::spawn(async move { run_n2c_handshake_server(&mut channel, &our_data).await });

        // Client opens in query mode.
        let their_data = N2CVersionData {
            network_magic: 2,
            query: true,
        };
        let proposal = encode_propose_versions_n2c(n2c::N2C_VERSIONS, &their_data);
        ingress_tx.send(Bytes::from(proposal)).await.unwrap();

        let result = handle.await.unwrap().unwrap();
        assert!(
            result.query,
            "server must flag a query-mode handshake as such"
        );

        let reply = egress_rx.recv().await.expect("server must reply");
        let mut dec = Decoder::new(&reply.2);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(
            dec.u64().unwrap(),
            MSG_QUERY_REPLY,
            "N2C handshake server must answer a query-mode proposal with \
             MsgQueryReply (tag 3), not MsgAcceptVersion (tag 1)"
        );
        let map_len = dec.map().unwrap().unwrap();
        assert_eq!(
            map_len,
            n2c::N2C_VERSIONS.len() as u64,
            "MsgQueryReply must carry the full supported-version table"
        );
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..map_len {
            let wire_version = dec.u16().unwrap();
            seen.insert(n2c::decode_n2c_version(wire_version));
            // Skip the version_data that follows each key.
            let _ = N2CVersionData::decode(&mut dec).unwrap();
        }
        for &v in n2c::N2C_VERSIONS {
            assert!(seen.contains(&v), "query reply missing version {v}");
        }
    }

    /// #1104: `run_n2n_handshake_server` computed the negotiated
    /// (`our_data.accept(their_data)`) version_data correctly but then
    /// discarded it and sent its own raw `our_data` in `MsgAcceptVersion`
    /// instead. Grounded in `Ouroboros.Network.Protocol.Handshake.Server`'s
    /// `handshakeServerPeer` (`acceptOrRefuse` -> `Accept agreedData` ->
    /// `Yield (MsgAcceptVersion vNumber (encodeData vNumber agreedData))`,
    /// IntersectMBO/ouroboros-network) — the wire reply carries the
    /// `Accept`-computed record, never a raw local or remote value.
    ///
    /// Drives the REAL server function through a `MuxChannel` (same idiom as
    /// `n2c_query_mode_server_replies_with_query_reply` above). The server
    /// declares `peer_sharing = true`; the client proposes
    /// `peer_sharing = false`. `peer_sharing` negotiates by AND
    /// (`N2NVersionData::accept`), so the correct accepted value is
    /// `false` — but the server's own raw `our_data.peer_sharing` is `true`.
    /// Before the fix this test fails (`our_data` sent unmodified, so the
    /// wire shows `true`); after the fix it passes.
    #[tokio::test]
    async fn n2n_accept_version_sends_negotiated_not_raw_local_data() {
        use bytes::Bytes;
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;
        use tokio::sync::mpsc;

        let (egress_tx, mut egress_rx) = mpsc::channel(8);
        let (ingress_tx, ingress_rx) = mpsc::channel(8);
        let mut channel = MuxChannel::new(
            0,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            65536,
            Arc::new(AtomicUsize::new(0)),
        );

        // Server: peer_sharing enabled, full duplex.
        let our_data = N2NVersionData::new(2, false, true);
        let our_data_for_check = our_data.clone();
        let handle =
            tokio::spawn(async move { run_n2n_handshake_server(&mut channel, &our_data).await });

        // Client: peer_sharing DISABLED, and initiator-only (a second field
        // whose negotiated value also differs from the server's raw data —
        // min/OR semantics, so the accepted value must be `true` even though
        // the server's own `initiator_only` is `false`).
        let their_data = N2NVersionData {
            network_magic: 2,
            initiator_only: true,
            peer_sharing: false,
            query: false,
        };
        let proposal = encode_propose_versions_n2n(n2n::N2N_VERSIONS, &their_data);
        ingress_tx.send(Bytes::from(proposal)).await.unwrap();

        let result = handle.await.unwrap().unwrap();
        assert!(!result.query);
        assert!(!result.simultaneous_open);

        let reply = egress_rx.recv().await.expect("server must reply");
        let mut dec = Decoder::new(&reply.2);
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u64().unwrap(), MSG_ACCEPT_VERSION);
        let _version = dec.u16().unwrap();
        let sent = N2NVersionData::decode(&mut dec).unwrap();

        // The value actually reachable via the real N2NVersionData::accept()
        // rule — this is what the wire MUST carry.
        let expected = our_data_for_check.accept(&their_data).unwrap();
        assert_eq!(
            sent, expected,
            "MsgAcceptVersion must carry the NEGOTIATED version_data, not our_data"
        );
        assert!(
            !sent.peer_sharing,
            "peer_sharing negotiates by AND(true, false) = false, but the \
             server's own raw peer_sharing is true — sending true would tell \
             the peer peer sharing is enabled when it is not"
        );
        assert!(
            sent.initiator_only,
            "initiator_only negotiates by min/OR(false, true) = true, but \
             the server's own raw initiator_only is false"
        );
        assert_ne!(
            sent, our_data_for_check,
            "the negotiated value must differ from our_data in this scenario \
             — otherwise this test cannot distinguish the fix from the bug"
        );
    }

    /// #1104 (N2C half): `run_n2c_handshake_server` has the identical
    /// discard-the-accepted-value pattern that `run_n2n_handshake_server` had
    /// (see `n2n_accept_version_sends_negotiated_not_raw_local_data` above).
    /// Fixed identically: use `our_data.accept(their_data)`'s result, not
    /// `our_data`, in the `MsgAcceptVersion` reply.
    ///
    /// Unlike N2N this is NOT currently wire-observable: `N2CVersionData` has
    /// exactly two fields, `network_magic` (must already be EQUAL to reach
    /// `Some` at all — `accept` early-returns `None` on any mismatch) and
    /// `query` (OR semantics, gated by a diversion into the `MsgQueryReply`
    /// arm the instant `their_data.query` is true — the one case where OR
    /// could raise the accepted value above `our_data.query` never reaches
    /// this code path). So `accepted == our_data` is a mathematical
    /// invariant of every reachable call today, and this test cannot fail
    /// under the pre-fix code — it instead pins the invariant this fix
    /// relies on. `assert_eq!(sent, our_data)` documents why, and
    /// `assert_eq!(sent, expected)` is the assertion that stops being
    /// vacuous the moment `N2CVersionData` grows a field with real
    /// min/AND/OR semantics, mirroring `N2NVersionData`.
    #[tokio::test]
    async fn n2c_accept_version_sends_negotiated_value() {
        use bytes::Bytes;
        use std::sync::atomic::AtomicUsize;
        use std::sync::Arc;
        use tokio::sync::mpsc;

        let (egress_tx, mut egress_rx) = mpsc::channel(8);
        let (ingress_tx, ingress_rx) = mpsc::channel(8);
        let mut channel = MuxChannel::new(
            0,
            crate::mux::Direction::ResponderDir,
            egress_tx,
            ingress_rx,
            65536,
            Arc::new(AtomicUsize::new(0)),
        );

        let our_data = N2CVersionData::new(2);
        let our_data_for_check = our_data.clone();
        let handle =
            tokio::spawn(async move { run_n2c_handshake_server(&mut channel, &our_data).await });

        let their_data = N2CVersionData {
            network_magic: 2,
            query: false,
        };
        let proposal = encode_propose_versions_n2c(n2c::N2C_VERSIONS, &their_data);
        ingress_tx.send(Bytes::from(proposal)).await.unwrap();

        let result = handle.await.unwrap().unwrap();
        assert!(!result.query);

        let reply = egress_rx.recv().await.expect("server must reply");
        let mut dec = Decoder::new(&reply.2);
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u64().unwrap(), MSG_ACCEPT_VERSION);
        let _wire_version = dec.u16().unwrap();
        let sent = N2CVersionData::decode(&mut dec).unwrap();

        let expected = our_data_for_check.accept(&their_data).unwrap();
        assert_eq!(
            sent, expected,
            "MsgAcceptVersion must carry the NEGOTIATED version_data"
        );
        assert_eq!(
            sent, our_data_for_check,
            "reachable N2C fields make accepted == our_data today (see doc comment)"
        );
    }

    #[test]
    fn simultaneous_open_negotiates_version() {
        // When we receive MsgProposeVersions (simultaneous open), the decoder should
        // negotiate the highest common version instead of returning version 0.
        let our_data = N2NVersionData::new(2, false, true);
        let their_data = N2NVersionData::new(2, false, true);
        let proposal = encode_propose_versions_n2n(n2n::N2N_VERSIONS, &their_data);
        let result = decode_handshake_response(&proposal, &our_data).unwrap();
        assert!(result.simultaneous_open);
        // Should negotiate the highest common version (first in N2N_VERSIONS preference order)
        assert_eq!(result.version, n2n::N2N_VERSIONS[0]);
        assert_ne!(result.version, 0, "version must not be the old sentinel 0");
    }

    #[test]
    fn simultaneous_open_version_mismatch() {
        // No common versions between the two sides — should return VersionMismatch.
        let our_data = N2NVersionData::new(2, false, true);
        // Build a proposal with a version we don't support (e.g., version 99)
        let fake_data = N2NVersionData::new(2, false, true);
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).expect("infallible");
        enc.u64(MSG_PROPOSE_VERSIONS).expect("infallible");
        enc.map(1).expect("infallible");
        enc.u16(99).expect("infallible");
        fake_data.encode(&mut enc);

        let result = decode_handshake_response(&buf, &our_data);
        assert!(
            matches!(result, Err(HandshakeError::VersionMismatch { .. })),
            "expected VersionMismatch, got: {result:?}"
        );
    }

    #[test]
    fn simultaneous_open_magic_mismatch() {
        // Same version numbers but different network magic — should fail.
        let our_data = N2NVersionData::new(2, false, true);
        let their_data = N2NVersionData::new(764824073, false, true); // mainnet magic
        let proposal = encode_propose_versions_n2n(n2n::N2N_VERSIONS, &their_data);
        let result = decode_handshake_response(&proposal, &our_data);
        assert!(
            matches!(result, Err(HandshakeError::NetworkMagicMismatch { .. })),
            "expected NetworkMagicMismatch, got: {result:?}"
        );
    }

    // ── A-003: handshake version map cap (security audit 2026-05-19) ─────────

    /// Build a raw MsgProposeVersions N2N CBOR with `n` map entries.
    fn build_n2n_propose_with_n_versions(n: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(MSG_PROPOSE_VERSIONS).unwrap();
        enc.map(n).unwrap();
        // Fill with `n` version entries (version=99, data=[2,false,false,false])
        for _ in 0..n {
            enc.u16(99).unwrap(); // unknown version key
            enc.array(4).unwrap();
            enc.u64(2).unwrap(); // network_magic
            enc.bool(false).unwrap(); // initiatorOnlyDiffusionMode
            enc.bool(false).unwrap(); // peerSharing
            enc.bool(false).unwrap(); // query
        }
        buf
    }

    /// Build a raw MsgProposeVersions N2C CBOR with `n` map entries.
    fn build_n2c_propose_with_n_versions(n: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(MSG_PROPOSE_VERSIONS).unwrap();
        enc.map(n).unwrap();
        for _ in 0..n {
            // N2C version with bit-15 set: wire version 0x8011 = logical 17
            enc.u16(0x8011).unwrap();
            enc.array(2).unwrap();
            enc.u64(2).unwrap(); // network_magic
            enc.bool(false).unwrap(); // query
        }
        buf
    }

    #[test]
    fn n2n_propose_within_version_cap_accepted() {
        // Exactly MAX_HANDSHAKE_VERSIONS entries — must decode OK.
        let buf = build_n2n_propose_with_n_versions(MAX_HANDSHAKE_VERSIONS);
        // All versions are unknown (99), so decoded map will be empty — that's fine.
        let result = decode_propose_versions_n2n(&buf);
        assert!(
            result.is_ok(),
            "version count == MAX should succeed, got: {result:?}"
        );
    }

    #[test]
    fn n2n_propose_exceeding_version_cap_rejected() {
        let buf = build_n2n_propose_with_n_versions(MAX_HANDSHAKE_VERSIONS + 1);
        let result = decode_propose_versions_n2n(&buf);
        assert!(
            matches!(result, Err(HandshakeError::DecodeError(_))),
            "version count > MAX must be rejected, got: {result:?}"
        );
    }

    #[test]
    fn n2c_propose_within_version_cap_accepted() {
        let buf = build_n2c_propose_with_n_versions(MAX_HANDSHAKE_VERSIONS);
        let result = decode_propose_versions_n2c(&buf);
        assert!(
            result.is_ok(),
            "N2C version count == MAX should succeed, got: {result:?}"
        );
    }

    #[test]
    fn n2c_propose_exceeding_version_cap_rejected() {
        let buf = build_n2c_propose_with_n_versions(MAX_HANDSHAKE_VERSIONS + 1);
        let result = decode_propose_versions_n2c(&buf);
        assert!(
            matches!(result, Err(HandshakeError::DecodeError(_))),
            "N2C version count > MAX must be rejected, got: {result:?}"
        );
    }

    /// Property test: version count lattice [0, MAX/2, MAX, MAX+1, MAX*4].
    #[test]
    fn n2n_version_count_lattice() {
        for n in [0u64, 1, MAX_HANDSHAKE_VERSIONS / 2, MAX_HANDSHAKE_VERSIONS] {
            let buf = build_n2n_propose_with_n_versions(n);
            let result = decode_propose_versions_n2n(&buf);
            assert!(
                result.is_ok(),
                "count {n} <= MAX should succeed; got: {result:?}"
            );
        }
        for n in [
            MAX_HANDSHAKE_VERSIONS + 1,
            MAX_HANDSHAKE_VERSIONS * 2,
            MAX_HANDSHAKE_VERSIONS * 4,
        ] {
            let buf = build_n2n_propose_with_n_versions(n);
            let result = decode_propose_versions_n2n(&buf);
            assert!(result.is_err(), "count {n} > MAX must fail; got: Ok(..)");
        }
    }

    // ── #1091: `ping -Q`/`--query-versions` full-table decode ────────────

    /// The common case: a real `MsgQueryReply` carrying every version the
    /// peer supports, each with its OWN `N2NVersionData` — this is what a
    /// real `cardano-node` sends (see `dugite-cli`'s `ping.rs` module doc
    /// for the live capture this is grounded in).
    #[test]
    fn decode_query_reply_n2n_full_table() {
        let mut data_by_version = BTreeMap::new();
        data_by_version.insert(n2n::N2N_V14, N2NVersionData::new(2, false, true));
        data_by_version.insert(n2n::N2N_V15, N2NVersionData::new(2, true, false));
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(MSG_QUERY_REPLY).unwrap();
        enc.map(data_by_version.len() as u64).unwrap();
        for (v, d) in &data_by_version {
            enc.u16(*v).unwrap();
            d.encode(&mut enc);
        }

        let versions = decode_query_reply_n2n(&buf).unwrap();
        assert_eq!(versions.len(), 2);
        let map: BTreeMap<u16, N2NVersionData> = versions.into_iter().collect();
        assert_eq!(map, data_by_version);
    }

    /// A server that does NOT implement query mode (dugite-node's own N2C
    /// handshake responder, at time of writing — see `ping.rs`'s module
    /// doc) replies with a plain `MsgAcceptVersion` instead of
    /// `MsgQueryReply`. Real `cardano-cli ping -Q` tolerates this and
    /// reports the one accepted version; `decode_query_reply_n2n` must do
    /// the same rather than erroring on an "unexpected tag".
    #[test]
    fn decode_query_reply_n2n_falls_back_to_plain_accept() {
        let data = N2NVersionData::new(2, true, false);
        let accept = encode_accept_version_n2n(n2n::N2N_V15, &data);
        let versions = decode_query_reply_n2n(&accept).unwrap();
        assert_eq!(versions, vec![(n2n::N2N_V15, data)]);
    }

    #[test]
    fn decode_query_reply_n2n_propagates_refuse() {
        let msg = encode_refuse_with_reason(2, n2n::N2N_V15, "nope");
        let result = decode_query_reply_n2n(&msg);
        assert!(matches!(result, Err(HandshakeError::Refused { .. })));
    }

    #[test]
    fn decode_query_reply_n2n_rejects_unexpected_tag() {
        let msg =
            encode_propose_versions_n2n(n2n::N2N_VERSIONS, &N2NVersionData::new(2, true, false));
        // A bare MsgProposeVersions (tag 0) is neither a reply nor an accept.
        let result = decode_query_reply_n2n(&msg);
        assert!(result.is_err());
    }

    /// As `decode_query_reply_n2n_full_table`, for N2C — including the
    /// bit-15 wire encoding round-trip.
    #[test]
    fn decode_query_reply_n2c_full_table() {
        let mut data_by_version = BTreeMap::new();
        data_by_version.insert(n2c::N2C_V16, N2CVersionData::new(2));
        data_by_version.insert(n2c::N2C_V23, N2CVersionData::new(2));
        let mut buf = Vec::new();
        let mut enc = Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u64(MSG_QUERY_REPLY).unwrap();
        enc.map(data_by_version.len() as u64).unwrap();
        for (v, d) in &data_by_version {
            enc.u16(n2c::encode_n2c_version(*v)).unwrap();
            d.encode(&mut enc);
        }

        let versions = decode_query_reply_n2c(&buf).unwrap();
        assert_eq!(versions.len(), 2);
        let map: BTreeMap<u16, N2CVersionData> = versions.into_iter().collect();
        assert_eq!(map, data_by_version);
    }

    /// Live-verified fallback (see `ping.rs`'s module doc): dugite-node's
    /// N2C handshake server does not implement query mode at all and
    /// always replies with a plain `MsgAcceptVersion`.
    #[test]
    fn decode_query_reply_n2c_falls_back_to_plain_accept() {
        let data = N2CVersionData::new(2);
        let accept = encode_accept_version_n2c(n2c::N2C_V23, &data);
        let versions = decode_query_reply_n2c(&accept).unwrap();
        assert_eq!(versions, vec![(n2c::N2C_V23, data)]);
    }

    #[tokio::test]
    async fn query_n2n_versions_over_a_real_channel() {
        let (mut client_ch, mut server_rx, server_tx) = make_test_mux_channel(PROTOCOL_HANDSHAKE);

        let data = N2NVersionData::new(9, true, false);
        let accept = encode_accept_version_n2n(n2n::N2N_V15, &data);
        let server = tokio::spawn(async move {
            let _proposal = server_rx.recv().await.unwrap();
            server_tx.send(bytes::Bytes::from(accept)).await.unwrap();
        });

        let versions = query_n2n_versions(&mut client_ch, 9).await.unwrap();
        assert_eq!(versions, vec![(n2n::N2N_V15, data)]);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn query_n2c_versions_over_a_real_channel() {
        let (mut client_ch, mut server_rx, server_tx) = make_test_mux_channel(PROTOCOL_HANDSHAKE);

        let mut table = BTreeMap::new();
        table.insert(n2c::N2C_V16, N2CVersionData::new(9));
        table.insert(n2c::N2C_V23, N2CVersionData::new(9));
        let mut reply = Vec::new();
        let mut enc = Encoder::new(&mut reply);
        enc.array(2).unwrap();
        enc.u64(MSG_QUERY_REPLY).unwrap();
        enc.map(table.len() as u64).unwrap();
        for (v, d) in &table {
            enc.u16(n2c::encode_n2c_version(*v)).unwrap();
            d.encode(&mut enc);
        }
        let server = tokio::spawn(async move {
            let _proposal = server_rx.recv().await.unwrap();
            server_tx.send(bytes::Bytes::from(reply)).await.unwrap();
        });

        let versions = query_n2c_versions(&mut client_ch, 9).await.unwrap();
        assert_eq!(versions.len(), 2);
        let map: BTreeMap<u16, N2CVersionData> = versions.into_iter().collect();
        assert_eq!(map, table);
        server.await.unwrap();
    }

    /// Minimal `MuxChannel` test harness, mirroring the one in
    /// `protocol/keepalive/client.rs`'s own tests: an mpsc pair standing in
    /// for the mux's egress/ingress routing.
    fn make_test_mux_channel(
        protocol_id: u16,
    ) -> (
        MuxChannel,
        tokio::sync::mpsc::Receiver<(u16, crate::mux::Direction, bytes::Bytes)>,
        tokio::sync::mpsc::Sender<bytes::Bytes>,
    ) {
        let (egress_tx, egress_rx) = tokio::sync::mpsc::channel(8);
        let (ingress_tx, ingress_rx) = tokio::sync::mpsc::channel(8);
        let channel = MuxChannel::new(
            protocol_id,
            crate::mux::Direction::InitiatorDir,
            egress_tx,
            ingress_rx,
            4 * 1024 * 1024,
            std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        );
        (channel, egress_rx, ingress_tx)
    }
}
