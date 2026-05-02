use crate::credentials::{Credential, Pointer, StakeReference};
use crate::hash::Hash28;
use crate::network::NetworkId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum AddressError {
    #[error("Invalid address header byte: {0:#04x}")]
    InvalidHeader(u8),
    #[error("Address too short")]
    TooShort,
    #[error("Invalid bech32 encoding: {0}")]
    Bech32Error(String),
    #[error("Invalid Byron address")]
    InvalidByronAddress,
}

/// Cardano address (all types)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Address {
    /// Base address: payment + staking credential
    Base(BaseAddress),
    /// Enterprise address: payment credential only (no staking)
    Enterprise(EnterpriseAddress),
    /// Pointer address: payment + stake pointer
    Pointer(PointerAddress),
    /// Reward/stake address (for withdrawals)
    Reward(RewardAddress),
    /// Byron-era bootstrap address
    Byron(ByronAddress),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BaseAddress {
    pub network: NetworkId,
    pub payment: Credential,
    pub stake: Credential,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EnterpriseAddress {
    pub network: NetworkId,
    pub payment: Credential,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PointerAddress {
    pub network: NetworkId,
    pub payment: Credential,
    pub pointer: Pointer,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RewardAddress {
    pub network: NetworkId,
    pub stake: Credential,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ByronAddress {
    pub payload: Vec<u8>,
}

impl ByronAddress {
    /// Return the serialized byte size of the address-attributes map embedded
    /// in this Byron/bootstrap address, or `None` if the payload cannot be
    /// decoded as a well-formed Byron CBOR address.
    ///
    /// Mirrors the Haskell function `bootstrapAttrsSize` used by the Shelley
    /// `OutputBootAddrAttrsTooBig` predicate. The Byron address is encoded as
    /// a 2-element CBOR array: `[ tag(24, bytes(payload_cbor)), crc32 ]`.
    /// Inside the inner `payload_cbor` is a 3-element array
    /// `[ root, attrs, addr_type ]`. This method measures the serialized
    /// length of the `attrs` element by walking the inner CBOR with minicbor
    /// and recording byte offsets — no reallocation/round-trip is required.
    pub fn attributes_byte_size(&self) -> Option<usize> {
        // The on-the-wire format always begins with CBOR array(2) (`0x82`).
        // The N2C/N2N decoders sometimes hand us a payload that has been
        // pre-stripped down to the inner bytes (legacy fixtures); accept
        // either shape by sniffing the first byte.
        let inner_bytes: &[u8] = if self.payload.first() == Some(&0x82) {
            // Outer array(2): [ tag24(payload_cbor), crc32 ]
            let mut d = minicbor::Decoder::new(&self.payload);
            d.array().ok()?;
            // tag(24) + bytes(payload_cbor)
            let tag = d.tag().ok()?;
            // Tag::Cbor (24) — minicbor exposes the raw u64.
            if tag.as_u64() != 24 {
                return None;
            }
            d.bytes().ok()?
        } else {
            &self.payload
        };

        // Inner payload: [ root (bytes), attrs (map), addr_type (int) ]
        let mut d = minicbor::Decoder::new(inner_bytes);
        d.array().ok()?;
        // Skip `root` (a 28-byte ByteString).
        d.bytes().ok()?;
        // Record attrs span by reading start/end positions on the decoder.
        let attrs_start = d.position();
        d.skip().ok()?;
        let attrs_end = d.position();
        Some(attrs_end - attrs_start)
    }
}

impl Address {
    /// Decode an address from raw bytes (Shelley or Byron format)
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, AddressError> {
        if bytes.is_empty() {
            return Err(AddressError::TooShort);
        }

        let header = bytes[0];

        // Byron addresses start with CBOR encoding (0x82, 0x83, etc.)
        // or have the Shelley-era type nibble 0b1000.
        // Detect Byron by checking if the first byte is a CBOR array/tag marker
        // that doesn't match Shelley header patterns.
        // CBOR major type 4 (array) starts at 0x80, major type 6 (tag) starts at 0xC0.
        // Byron addresses are typically CBOR arrays starting with 0x82 or 0x83.
        if header == 0x82 || header == 0x83 {
            return Ok(Address::Byron(ByronAddress {
                payload: bytes.to_vec(),
            }));
        }

        let addr_type = (header >> 4) & 0x0F;
        let network_id =
            NetworkId::from_u8(header & 0x0F).ok_or(AddressError::InvalidHeader(header))?;

        match addr_type {
            // Base addresses (types 0-3)
            0b0000..=0b0011 => {
                if bytes.len() < 57 {
                    return Err(AddressError::TooShort);
                }
                let payment = decode_credential(addr_type & 0b01, &bytes[1..29])?;
                let stake = decode_credential((addr_type >> 1) & 0b01, &bytes[29..57])?;
                Ok(Address::Base(BaseAddress {
                    network: network_id,
                    payment,
                    stake,
                }))
            }
            // Pointer addresses (types 4-5)
            0b0100..=0b0101 => {
                if bytes.len() < 29 {
                    return Err(AddressError::TooShort);
                }
                let payment = decode_credential(addr_type & 0b01, &bytes[1..29])?;
                let (pointer, _) = decode_pointer(&bytes[29..])?;
                Ok(Address::Pointer(PointerAddress {
                    network: network_id,
                    payment,
                    pointer,
                }))
            }
            // Enterprise addresses (types 6-7)
            0b0110..=0b0111 => {
                if bytes.len() < 29 {
                    return Err(AddressError::TooShort);
                }
                let payment = decode_credential(addr_type & 0b01, &bytes[1..29])?;
                Ok(Address::Enterprise(EnterpriseAddress {
                    network: network_id,
                    payment,
                }))
            }
            // Byron address (type 8)
            0b1000 => Ok(Address::Byron(ByronAddress {
                payload: bytes.to_vec(),
            })),
            // Reward addresses (types 14-15)
            0b1110 | 0b1111 => {
                if bytes.len() < 29 {
                    return Err(AddressError::TooShort);
                }
                let stake = decode_credential(addr_type & 0b01, &bytes[1..29])?;
                Ok(Address::Reward(RewardAddress {
                    network: network_id,
                    stake,
                }))
            }
            _ => Err(AddressError::InvalidHeader(header)),
        }
    }

    /// Serialize address to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Address::Base(addr) => {
                let payment_bit = credential_type_bit(&addr.payment);
                let stake_bit = credential_type_bit(&addr.stake);
                let header = (stake_bit << 5) | (payment_bit << 4) | addr.network.to_u8();
                let mut bytes = vec![header];
                bytes.extend_from_slice(addr.payment.to_hash().as_bytes());
                bytes.extend_from_slice(addr.stake.to_hash().as_bytes());
                bytes
            }
            Address::Enterprise(addr) => {
                let payment_bit = credential_type_bit(&addr.payment);
                let header = (0b0110 | payment_bit) << 4 | addr.network.to_u8();
                let mut bytes = vec![header];
                bytes.extend_from_slice(addr.payment.to_hash().as_bytes());
                bytes
            }
            Address::Reward(addr) => {
                let stake_bit = credential_type_bit(&addr.stake);
                let header = (0b1110 | stake_bit) << 4 | addr.network.to_u8();
                let mut bytes = vec![header];
                bytes.extend_from_slice(addr.stake.to_hash().as_bytes());
                bytes
            }
            Address::Pointer(addr) => {
                let payment_bit = credential_type_bit(&addr.payment);
                let header = (0b0100 | payment_bit) << 4 | addr.network.to_u8();
                let mut bytes = vec![header];
                bytes.extend_from_slice(addr.payment.to_hash().as_bytes());
                bytes.extend(encode_variable_length(addr.pointer.slot));
                bytes.extend(encode_variable_length(addr.pointer.tx_index));
                bytes.extend(encode_variable_length(addr.pointer.cert_index));
                bytes
            }
            Address::Byron(addr) => addr.payload.clone(),
        }
    }

    pub fn network_id(&self) -> Option<NetworkId> {
        match self {
            Address::Base(a) => Some(a.network),
            Address::Enterprise(a) => Some(a.network),
            Address::Pointer(a) => Some(a.network),
            Address::Reward(a) => Some(a.network),
            Address::Byron(_) => None,
        }
    }

    pub fn payment_credential(&self) -> Option<&Credential> {
        match self {
            Address::Base(a) => Some(&a.payment),
            Address::Enterprise(a) => Some(&a.payment),
            Address::Pointer(a) => Some(&a.payment),
            Address::Reward(_) => None,
            Address::Byron(_) => None,
        }
    }

    pub fn stake_reference(&self) -> StakeReference {
        match self {
            Address::Base(a) => StakeReference::StakeCredential(a.stake.clone()),
            Address::Pointer(a) => StakeReference::Pointer(a.pointer),
            _ => StakeReference::Null,
        }
    }
}

fn credential_type_bit(cred: &Credential) -> u8 {
    match cred {
        Credential::VerificationKey(_) => 0,
        Credential::Script(_) => 1,
    }
}

fn decode_credential(type_bit: u8, bytes: &[u8]) -> Result<Credential, AddressError> {
    if bytes.len() < 28 {
        return Err(AddressError::TooShort);
    }
    let mut hash = [0u8; 28];
    hash.copy_from_slice(&bytes[..28]);
    let h = Hash28::from_bytes(hash);
    match type_bit {
        0 => Ok(Credential::VerificationKey(h)),
        1 => Ok(Credential::Script(h)),
        _ => Err(AddressError::InvalidHeader(type_bit)),
    }
}

fn decode_pointer(bytes: &[u8]) -> Result<(Pointer, usize), AddressError> {
    let (slot, n1) = decode_variable_length(bytes).ok_or(AddressError::TooShort)?;
    let (tx_index, n2) = decode_variable_length(&bytes[n1..]).ok_or(AddressError::TooShort)?;
    let (cert_index, n3) =
        decode_variable_length(&bytes[n1 + n2..]).ok_or(AddressError::TooShort)?;
    Ok((
        Pointer {
            slot,
            tx_index,
            cert_index,
        },
        n1 + n2 + n3,
    ))
}

fn decode_variable_length(bytes: &[u8]) -> Option<(u64, usize)> {
    let mut result: u64 = 0;
    for (i, &byte) in bytes.iter().enumerate() {
        result = (result << 7) | (byte & 0x7F) as u64;
        if byte & 0x80 == 0 {
            return Some((result, i + 1));
        }
    }
    None
}

fn encode_variable_length(mut value: u64) -> Vec<u8> {
    if value == 0 {
        return vec![0];
    }
    let mut bytes = Vec::new();
    while value > 0 {
        bytes.push((value & 0x7F) as u8);
        value >>= 7;
    }
    bytes.reverse();
    let last = bytes.len() - 1;
    for b in bytes.iter_mut().take(last) {
        *b |= 0x80;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hash28(val: u8) -> Hash28 {
        Hash28::from_bytes([val; 28])
    }

    #[test]
    fn test_base_address_roundtrip() {
        let addr = Address::Base(BaseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(make_hash28(0xaa)),
            stake: Credential::VerificationKey(make_hash28(0xbb)),
        });
        let bytes = addr.to_bytes();
        assert_eq!(bytes.len(), 57);
        // Header: type 0b0000, network 0x00
        assert_eq!(bytes[0] & 0xF0, 0x00);
        assert_eq!(bytes[0] & 0x0F, 0x00);

        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_base_address_script_credentials() {
        let addr = Address::Base(BaseAddress {
            network: NetworkId::Mainnet,
            payment: Credential::Script(make_hash28(0xcc)),
            stake: Credential::Script(make_hash28(0xdd)),
        });
        let bytes = addr.to_bytes();
        // type=0b0011 (both script), network=1
        assert_eq!(bytes[0], 0x31);
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_enterprise_address_roundtrip() {
        let addr = Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(make_hash28(0xee)),
        });
        let bytes = addr.to_bytes();
        assert_eq!(bytes.len(), 29);
        // type=0b0110, network=0
        assert_eq!(bytes[0], 0x60);
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_reward_address_roundtrip() {
        let addr = Address::Reward(RewardAddress {
            network: NetworkId::Mainnet,
            stake: Credential::VerificationKey(make_hash28(0xff)),
        });
        let bytes = addr.to_bytes();
        assert_eq!(bytes.len(), 29);
        // type=0b1110, network=1
        assert_eq!(bytes[0], 0xe1);
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_pointer_address_roundtrip() {
        let addr = Address::Pointer(PointerAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(make_hash28(0x11)),
            pointer: Pointer {
                slot: 100,
                tx_index: 2,
                cert_index: 0,
            },
        });
        let bytes = addr.to_bytes();
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_pointer_address_large_values() {
        let addr = Address::Pointer(PointerAddress {
            network: NetworkId::Mainnet,
            payment: Credential::VerificationKey(make_hash28(0x22)),
            pointer: Pointer {
                slot: 100_000_000,
                tx_index: 300,
                cert_index: 50,
            },
        });
        let bytes = addr.to_bytes();
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_byron_address() {
        // Byron addresses start with 0x82 or 0x83
        let byron_bytes = vec![0x82, 0x01, 0x02, 0x03];
        let addr = Address::from_bytes(&byron_bytes).unwrap();
        match addr {
            Address::Byron(b) => assert_eq!(b.payload, byron_bytes),
            other => panic!("Expected Byron, got {other:?}"),
        }
    }

    #[test]
    fn test_empty_address_error() {
        assert!(Address::from_bytes(&[]).is_err());
    }

    #[test]
    fn test_too_short_base_address() {
        // Base address needs 57 bytes, provide only 30
        let mut bytes = vec![0x00]; // type 0, testnet
        bytes.extend_from_slice(&[0xaa; 28]); // payment only, missing stake
        assert!(Address::from_bytes(&bytes).is_err());
    }

    #[test]
    fn test_too_short_enterprise_address() {
        let bytes = vec![0x60, 0xaa]; // type 6, testnet, only 1 byte of hash
        assert!(Address::from_bytes(&bytes).is_err());
    }

    #[test]
    fn test_network_id() {
        let base = Address::Base(BaseAddress {
            network: NetworkId::Mainnet,
            payment: Credential::VerificationKey(make_hash28(0)),
            stake: Credential::VerificationKey(make_hash28(0)),
        });
        assert_eq!(base.network_id(), Some(NetworkId::Mainnet));

        let byron = Address::Byron(ByronAddress {
            payload: vec![0x82],
        });
        assert_eq!(byron.network_id(), None);
    }

    #[test]
    fn test_payment_credential() {
        let hash = make_hash28(0xab);
        let addr = Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(hash),
        });
        assert_eq!(
            addr.payment_credential(),
            Some(&Credential::VerificationKey(hash))
        );

        let reward = Address::Reward(RewardAddress {
            network: NetworkId::Testnet,
            stake: Credential::VerificationKey(hash),
        });
        assert_eq!(reward.payment_credential(), None);
    }

    #[test]
    fn test_stake_reference() {
        let hash = make_hash28(0xcd);
        let base = Address::Base(BaseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(make_hash28(0)),
            stake: Credential::VerificationKey(hash),
        });
        match base.stake_reference() {
            StakeReference::StakeCredential(c) => {
                assert_eq!(c, Credential::VerificationKey(hash));
            }
            other => panic!("Expected StakeCredential, got {other:?}"),
        }

        let enterprise = Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(make_hash28(0)),
        });
        assert!(matches!(enterprise.stake_reference(), StakeReference::Null));
    }

    #[test]
    fn test_variable_length_encoding_zero() {
        let encoded = encode_variable_length(0);
        assert_eq!(encoded, vec![0]);
        let (decoded, len) = decode_variable_length(&encoded).unwrap();
        assert_eq!(decoded, 0);
        assert_eq!(len, 1);
    }

    #[test]
    fn test_variable_length_encoding_small() {
        let encoded = encode_variable_length(127);
        assert_eq!(encoded, vec![0x7F]);
        let (decoded, len) = decode_variable_length(&encoded).unwrap();
        assert_eq!(decoded, 127);
        assert_eq!(len, 1);
    }

    #[test]
    fn test_variable_length_encoding_two_bytes() {
        let encoded = encode_variable_length(128);
        assert_eq!(encoded, vec![0x81, 0x00]);
        let (decoded, _) = decode_variable_length(&encoded).unwrap();
        assert_eq!(decoded, 128);
    }

    #[test]
    fn test_variable_length_encoding_large() {
        let value = 100_000_000u64;
        let encoded = encode_variable_length(value);
        let (decoded, _) = decode_variable_length(&encoded).unwrap();
        assert_eq!(decoded, value);
    }

    // -----------------------------------------------------------------------
    // Additional address tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_enterprise_address_serialization_roundtrip_script() {
        // Enterprise address with script credential
        let addr = Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Mainnet,
            payment: Credential::Script(make_hash28(0xdd)),
        });
        let bytes = addr.to_bytes();
        assert_eq!(bytes.len(), 29);
        // type=0b0111, network=1 -> 0x71
        assert_eq!(bytes[0], 0x71);
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_base_address_roundtrip_mixed_credentials() {
        // Payment=VK, Stake=Script
        let addr = Address::Base(BaseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(make_hash28(0x11)),
            stake: Credential::Script(make_hash28(0x22)),
        });
        let bytes = addr.to_bytes();
        assert_eq!(bytes.len(), 57);
        // type=0b0010 (payment=VK=0, stake=Script=1), network=0 -> 0x20
        assert_eq!(bytes[0], 0x20);
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_reward_address_roundtrip_script() {
        let addr = Address::Reward(RewardAddress {
            network: NetworkId::Testnet,
            stake: Credential::Script(make_hash28(0xcc)),
        });
        let bytes = addr.to_bytes();
        assert_eq!(bytes.len(), 29);
        // type=0b1111, network=0 -> 0xf0
        assert_eq!(bytes[0], 0xf0);
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_network_id_extraction_mainnet() {
        let addr = Address::Enterprise(EnterpriseAddress {
            network: NetworkId::Mainnet,
            payment: Credential::VerificationKey(make_hash28(0xaa)),
        });
        assert_eq!(addr.network_id(), Some(NetworkId::Mainnet));

        let bytes = addr.to_bytes();
        // Network ID is in low nibble of header byte
        assert_eq!(bytes[0] & 0x0F, 1); // Mainnet = 1
    }

    #[test]
    fn test_network_id_extraction_testnet() {
        let addr = Address::Base(BaseAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(make_hash28(0xaa)),
            stake: Credential::VerificationKey(make_hash28(0xbb)),
        });
        assert_eq!(addr.network_id(), Some(NetworkId::Testnet));

        let bytes = addr.to_bytes();
        assert_eq!(bytes[0] & 0x0F, 0); // Testnet = 0
    }

    #[test]
    fn test_byron_address_identification() {
        // Byron addresses start with 0x82 or 0x83
        let byron_82 = Address::from_bytes(&[0x82, 0xd8, 0x18, 0x58]).unwrap();
        assert!(matches!(byron_82, Address::Byron(_)));
        assert_eq!(byron_82.network_id(), None);
        assert_eq!(byron_82.payment_credential(), None);

        let byron_83 = Address::from_bytes(&[0x83, 0x00, 0x01, 0x02]).unwrap();
        assert!(matches!(byron_83, Address::Byron(_)));
    }

    #[test]
    fn test_byron_address_roundtrip() {
        let raw = vec![0x82, 0xd8, 0x18, 0x58, 0x20, 0xAA, 0xBB];
        let addr = Address::from_bytes(&raw).unwrap();
        let bytes = addr.to_bytes();
        assert_eq!(bytes, raw);
    }

    #[test]
    fn test_address_type_8_byron() {
        // Type nibble 0b1000 (0x80 | network) should be Byron
        let mut bytes = vec![0x80]; // type 8, network 0
        bytes.extend_from_slice(&[0u8; 56]); // enough padding
        let addr = Address::from_bytes(&bytes).unwrap();
        assert!(matches!(addr, Address::Byron(_)));
    }

    #[test]
    fn test_invalid_address_type() {
        // Type nibble 0b1001 (9) is not a valid Shelley address type
        let mut bytes = vec![0x90]; // type 9, network 0
        bytes.extend_from_slice(&[0u8; 56]);
        let result = Address::from_bytes(&bytes);
        assert!(result.is_err());
    }

    #[test]
    fn test_pointer_address_zero_values() {
        let addr = Address::Pointer(PointerAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(make_hash28(0x33)),
            pointer: Pointer {
                slot: 0,
                tx_index: 0,
                cert_index: 0,
            },
        });
        let bytes = addr.to_bytes();
        let decoded = Address::from_bytes(&bytes).unwrap();
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_all_address_types_stake_reference() {
        let hash = make_hash28(0x55);

        // Reward address has Null stake reference
        let reward = Address::Reward(RewardAddress {
            network: NetworkId::Testnet,
            stake: Credential::VerificationKey(hash),
        });
        assert!(matches!(reward.stake_reference(), StakeReference::Null));

        // Byron address has Null stake reference
        let byron = Address::Byron(ByronAddress {
            payload: vec![0x82],
        });
        assert!(matches!(byron.stake_reference(), StakeReference::Null));

        // Pointer address has Pointer stake reference
        let pointer = Address::Pointer(PointerAddress {
            network: NetworkId::Testnet,
            payment: Credential::VerificationKey(hash),
            pointer: Pointer {
                slot: 42,
                tx_index: 1,
                cert_index: 0,
            },
        });
        match pointer.stake_reference() {
            StakeReference::Pointer(p) => {
                assert_eq!(p.slot, 42);
                assert_eq!(p.tx_index, 1);
                assert_eq!(p.cert_index, 0);
            }
            other => panic!("Expected Pointer, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------------
    // Byron `attributes_byte_size` — used by `OutputBootAddrAttrsTooBig`.
    //
    // Build a synthetic Byron address with a controllable `attrs` map size
    // and confirm we measure the same number of bytes the Haskell side
    // would count via `bootstrapAttrsSize`.
    // ---------------------------------------------------------------------

    /// Encode a synthetic Byron address whose `attrs` map carries a single
    /// attribute (key `0x01`, "DerivationPath") with a payload of
    /// `attr_payload_len` arbitrary bytes. Returns the full
    /// outer-CBOR-encoded address bytes.
    fn synth_byron_addr(attr_payload_len: usize) -> Vec<u8> {
        // Inner payload: [ root(28-byte bs), { 1 => bs(payload) }, addr_type=0 ]
        let mut inner = Vec::new();
        let mut e = minicbor::Encoder::new(&mut inner);
        e.array(3).unwrap();
        e.bytes(&[0u8; 28]).unwrap();
        e.map(1).unwrap();
        e.u8(1).unwrap();
        let attr_payload = vec![0xAAu8; attr_payload_len];
        e.bytes(&attr_payload).unwrap();
        e.u8(0).unwrap(); // AddrType::PubKey

        // Outer: [ tag(24, bytes(inner_cbor)), crc32_u32 ]
        let mut outer = Vec::new();
        let mut oe = minicbor::Encoder::new(&mut outer);
        oe.array(2).unwrap();
        oe.tag(minicbor::data::Tag::new(24)).unwrap();
        oe.bytes(&inner).unwrap();
        oe.u32(0xDEAD_BEEF).unwrap(); // crc32 placeholder, value irrelevant
        outer
    }

    #[test]
    fn test_byron_attributes_byte_size_basic() {
        // A 10-byte attribute payload encodes as: map(1) | u8(1) | bytes(10)
        // CBOR: 0xA1 (map with 1 pair)
        //       0x01 (u8 key)
        //       0x4A (bytes header for length 10)
        //       <10 bytes>
        // Total = 1 + 1 + 1 + 10 = 13.
        let addr_bytes = synth_byron_addr(10);
        let addr = ByronAddress {
            payload: addr_bytes,
        };
        assert_eq!(addr.attributes_byte_size(), Some(13));
    }

    #[test]
    fn test_byron_attributes_byte_size_oversized() {
        // 100-byte payload: map(1) | u8 | bytes header(2 bytes for 100) | 100
        // CBOR length-100 byte string header is 0x58 0x64 (2 bytes).
        // Total = 1 + 1 + 2 + 100 = 104.
        let addr_bytes = synth_byron_addr(100);
        let addr = ByronAddress {
            payload: addr_bytes,
        };
        assert_eq!(addr.attributes_byte_size(), Some(104));
    }

    #[test]
    fn test_byron_attributes_byte_size_empty() {
        // Empty attrs map encodes as 0xA0 (1 byte total).
        let mut inner = Vec::new();
        let mut e = minicbor::Encoder::new(&mut inner);
        e.array(3).unwrap();
        e.bytes(&[0u8; 28]).unwrap();
        e.map(0).unwrap();
        e.u8(0).unwrap();

        let mut outer = Vec::new();
        let mut oe = minicbor::Encoder::new(&mut outer);
        oe.array(2).unwrap();
        oe.tag(minicbor::data::Tag::new(24)).unwrap();
        oe.bytes(&inner).unwrap();
        oe.u32(0).unwrap();

        let addr = ByronAddress { payload: outer };
        assert_eq!(addr.attributes_byte_size(), Some(1));
    }
}
