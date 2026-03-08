//! PeerSharing mini-protocol (Ouroboros)
//!
//! Allows peers to exchange known peer addresses for decentralized peer discovery.
//! This implements the responder side — when a remote peer requests peers, we
//! respond with known shareable addresses.
//!
//! Protocol ID: 10 (N2N PeerSharing)
//!
//! Message flow:
//!   Client (initiator)         Server (responder)
//!   StIdle:
//!     MsgShareRequest(amount) →
//!                              ← MsgSharePeers(Vec<PeerAddress>)
//!   StIdle:
//!     MsgDone →
//!   StDone

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// PeerSharing protocol state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerSharingState {
    /// Client has agency — can send MsgShareRequest or MsgDone
    StIdle,
    /// Server has agency — must respond with MsgSharePeers
    StBusy,
    /// Terminal state
    StDone,
}

/// A peer address as exchanged in the PeerSharing protocol.
///
/// Cardano's PeerSharing uses a tagged representation:
///   [0, [ipv4_bytes, port]] for IPv4
///   [1, [ipv6_bytes, port]] for IPv6
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerAddress {
    IPv4(Ipv4Addr, u16),
    IPv6(Ipv6Addr, u16),
}

impl PeerAddress {
    /// Convert to a standard SocketAddr
    pub fn to_socket_addr(&self) -> SocketAddr {
        match self {
            PeerAddress::IPv4(ip, port) => SocketAddr::new(IpAddr::V4(*ip), *port),
            PeerAddress::IPv6(ip, port) => SocketAddr::new(IpAddr::V6(*ip), *port),
        }
    }

    /// Create from a SocketAddr
    pub fn from_socket_addr(addr: SocketAddr) -> Self {
        match addr {
            SocketAddr::V4(v4) => PeerAddress::IPv4(*v4.ip(), v4.port()),
            SocketAddr::V6(v6) => PeerAddress::IPv6(*v6.ip(), v6.port()),
        }
    }
}

/// PeerSharing protocol messages
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSharingMessage {
    /// Request up to `amount` peers from the remote
    ShareRequest(u8),
    /// Response with a list of peer addresses
    SharePeers(Vec<PeerAddress>),
    /// Terminate the protocol
    Done,
}

/// Encode a PeerSharingMessage to CBOR bytes.
///
/// Wire format (matching cardano-node):
///   MsgShareRequest: [0, amount]
///   MsgSharePeers:   [1, [[tag, [addr_bytes, port]], ...]]
///   MsgDone:         [2]
pub fn encode_message(msg: &PeerSharingMessage) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);

    match msg {
        PeerSharingMessage::ShareRequest(amount) => {
            enc.array(2).map_err(|e| e.to_string())?;
            enc.u32(0).map_err(|e| e.to_string())?;
            enc.u8(*amount).map_err(|e| e.to_string())?;
        }
        PeerSharingMessage::SharePeers(peers) => {
            enc.array(2).map_err(|e| e.to_string())?;
            enc.u32(1).map_err(|e| e.to_string())?;
            enc.array(peers.len() as u64).map_err(|e| e.to_string())?;
            for peer in peers {
                encode_peer_address(&mut enc, peer)?;
            }
        }
        PeerSharingMessage::Done => {
            enc.array(1).map_err(|e| e.to_string())?;
            enc.u32(2).map_err(|e| e.to_string())?;
        }
    }

    Ok(buf)
}

/// Decode a PeerSharingMessage from CBOR bytes.
pub fn decode_message(payload: &[u8]) -> Result<PeerSharingMessage, String> {
    let mut decoder = minicbor::Decoder::new(payload);
    let _arr_len = decoder.array().map_err(|e| e.to_string())?;
    let tag = decoder.u32().map_err(|e| e.to_string())?;

    match tag {
        // MsgShareRequest: [0, amount]
        0 => {
            let amount = decoder.u8().map_err(|e| e.to_string())?;
            Ok(PeerSharingMessage::ShareRequest(amount))
        }
        // MsgSharePeers: [1, [peer_addresses...]]
        1 => {
            let peer_count = decoder.array().map_err(|e| e.to_string())?.unwrap_or(0);
            let mut peers = Vec::with_capacity(peer_count as usize);
            for _ in 0..peer_count {
                let peer = decode_peer_address(&mut decoder)?;
                peers.push(peer);
            }
            Ok(PeerSharingMessage::SharePeers(peers))
        }
        // MsgDone: [2]
        2 => Ok(PeerSharingMessage::Done),
        other => Err(format!("Unknown PeerSharing message tag: {other}")),
    }
}

/// Encode a PeerAddress to CBOR.
///
/// Format: [tag, [addr_bytes, port]]
///   tag 0 = IPv4 (4-byte address)
///   tag 1 = IPv6 (16-byte address)
fn encode_peer_address(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    addr: &PeerAddress,
) -> Result<(), String> {
    enc.array(2).map_err(|e| e.to_string())?;
    match addr {
        PeerAddress::IPv4(ip, port) => {
            enc.u32(0).map_err(|e| e.to_string())?;
            enc.array(2).map_err(|e| e.to_string())?;
            enc.bytes(&ip.octets()).map_err(|e| e.to_string())?;
            enc.u16(*port).map_err(|e| e.to_string())?;
        }
        PeerAddress::IPv6(ip, port) => {
            enc.u32(1).map_err(|e| e.to_string())?;
            enc.array(2).map_err(|e| e.to_string())?;
            enc.bytes(&ip.octets()).map_err(|e| e.to_string())?;
            enc.u16(*port).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Decode a PeerAddress from CBOR.
fn decode_peer_address(decoder: &mut minicbor::Decoder) -> Result<PeerAddress, String> {
    let _arr = decoder.array().map_err(|e| e.to_string())?;
    let tag = decoder.u32().map_err(|e| e.to_string())?;

    let _inner_arr = decoder.array().map_err(|e| e.to_string())?;
    let addr_bytes = decoder.bytes().map_err(|e| e.to_string())?;
    let port = decoder.u16().map_err(|e| e.to_string())?;

    match tag {
        0 => {
            if addr_bytes.len() != 4 {
                return Err(format!("Invalid IPv4 address length: {}", addr_bytes.len()));
            }
            let ip = Ipv4Addr::new(addr_bytes[0], addr_bytes[1], addr_bytes[2], addr_bytes[3]);
            Ok(PeerAddress::IPv4(ip, port))
        }
        1 => {
            if addr_bytes.len() != 16 {
                return Err(format!("Invalid IPv6 address length: {}", addr_bytes.len()));
            }
            let mut octets = [0u8; 16];
            octets.copy_from_slice(addr_bytes);
            let ip = Ipv6Addr::from(octets);
            Ok(PeerAddress::IPv6(ip, port))
        }
        other => Err(format!("Unknown PeerAddress tag: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_address_ipv4_roundtrip() {
        let addr = PeerAddress::IPv4(Ipv4Addr::new(192, 168, 1, 100), 3001);
        let socket = addr.to_socket_addr();
        assert_eq!(socket.to_string(), "192.168.1.100:3001");

        let back = PeerAddress::from_socket_addr(socket);
        assert_eq!(addr, back);
    }

    #[test]
    fn test_peer_address_ipv6_roundtrip() {
        let addr = PeerAddress::IPv6(Ipv6Addr::LOCALHOST, 3001);
        let socket = addr.to_socket_addr();
        assert_eq!(socket.to_string(), "[::1]:3001");

        let back = PeerAddress::from_socket_addr(socket);
        assert_eq!(addr, back);
    }

    #[test]
    fn test_encode_decode_share_request() {
        let msg = PeerSharingMessage::ShareRequest(10);
        let encoded = encode_message(&msg).unwrap();
        let decoded = decode_message(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_encode_decode_share_peers_empty() {
        let msg = PeerSharingMessage::SharePeers(vec![]);
        let encoded = encode_message(&msg).unwrap();
        let decoded = decode_message(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_encode_decode_share_peers_ipv4() {
        let msg = PeerSharingMessage::SharePeers(vec![
            PeerAddress::IPv4(Ipv4Addr::new(1, 2, 3, 4), 3001),
            PeerAddress::IPv4(Ipv4Addr::new(10, 0, 0, 1), 3002),
        ]);
        let encoded = encode_message(&msg).unwrap();
        let decoded = decode_message(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_encode_decode_share_peers_ipv6() {
        let msg = PeerSharingMessage::SharePeers(vec![PeerAddress::IPv6(
            Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1),
            3001,
        )]);
        let encoded = encode_message(&msg).unwrap();
        let decoded = decode_message(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_encode_decode_share_peers_mixed() {
        let msg = PeerSharingMessage::SharePeers(vec![
            PeerAddress::IPv4(Ipv4Addr::new(192, 168, 1, 1), 3001),
            PeerAddress::IPv6(Ipv6Addr::LOCALHOST, 3002),
        ]);
        let encoded = encode_message(&msg).unwrap();
        let decoded = decode_message(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_encode_decode_done() {
        let msg = PeerSharingMessage::Done;
        let encoded = encode_message(&msg).unwrap();
        let decoded = decode_message(&encoded).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn test_decode_unknown_tag() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(1).unwrap();
        enc.u32(99).unwrap();
        let result = decode_message(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_socket_addr_v4() {
        let socket: SocketAddr = "127.0.0.1:3001".parse().unwrap();
        let peer = PeerAddress::from_socket_addr(socket);
        assert!(matches!(peer, PeerAddress::IPv4(_, 3001)));
    }

    #[test]
    fn test_from_socket_addr_v6() {
        let socket: SocketAddr = "[::1]:3001".parse().unwrap();
        let peer = PeerAddress::from_socket_addr(socket);
        assert!(matches!(peer, PeerAddress::IPv6(_, 3001)));
    }

    #[test]
    fn test_state_variants() {
        // Ensure state enum variants exist and are distinct
        assert_ne!(PeerSharingState::StIdle, PeerSharingState::StBusy);
        assert_ne!(PeerSharingState::StBusy, PeerSharingState::StDone);
        assert_ne!(PeerSharingState::StIdle, PeerSharingState::StDone);
    }
}
