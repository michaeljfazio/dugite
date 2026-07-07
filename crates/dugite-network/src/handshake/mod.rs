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
            if let Some(_accepted) = our_data.accept(their_data) {
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
                // Send MsgAcceptVersion
                let msg = encode_accept_version_n2n(our_version, our_data);
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
    decode_handshake_response_n2c(&response)
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
            if let Some(_accepted) = our_data.accept(their_data) {
                let msg = encode_accept_version_n2c(our_version, our_data);
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
fn decode_handshake_response_n2c(data: &[u8]) -> Result<HandshakeResult, HandshakeError> {
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
            let _ = N2CVersionData::decode(&mut dec);
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
        let result = decode_handshake_response_n2c(&encoded).unwrap();
        assert_eq!(result.version, 22); // logical, not wire
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
}
