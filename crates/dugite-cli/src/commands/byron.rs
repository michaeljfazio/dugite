//! Byron key conversion subcommands.
//!
//! Matches the `cardano-cli byron key <subcommand>` surface:
//!
//! | subcommand                  | action                                                         |
//! |-----------------------------|----------------------------------------------------------------|
//! | `signing-key-address`       | Derive a Byron base58 address from a signing key               |
//! | `signing-key-public`        | Extract the public verification key from a signing key         |
//! | `migrate-delegate-key-from` | Re-wrap a Byron-legacy delegate key into the current envelope  |
//! | `convert-byron-key`         | Convert a Byron signing key to `Ed25519BIP32` Shelley envelope |
//! | `convert-byron-genesis-vkey`| Convert a Byron genesis vkey to `GenesisUTxOVerificationKey`   |
//!
//! ## Byron Ed25519-BIP32 key format
//!
//! All Byron signing keys are 96-byte blobs:
//! - bytes  0-63: Ed25519 extended private key (clamped scalar || nonce)
//! - bytes 64-95: 32-byte chain code
//!
//! The old "legacy" format (cardano-sl ≤ 1.17) stores a 128-byte blob where
//! the first 64 bytes are the same extended key and bytes 64-127 are another
//! 64-byte value (public key || chain code). We extract 64 || chain_code as
//! the canonical 96 bytes.
//!
//! ## Byron public key format
//!
//! The verification key (vkey) is 64 bytes:
//! - bytes  0-31: Ed25519 public key point (compressed)
//! - bytes 32-63: 32-byte chain code
//!
//! ## Byron address construction (PubKey address)
//!
//! ```text
//! spending_data = array(2)[i64(0), bstr(64)(pubkey_concat_chaincode)]
//! attributes    = map(0)  // mainnet; testnet: map(1){ 2 => bstr(cbor(magic)) }
//! addr_spec     = array(3)[u8(0), spending_data, attributes]
//! root          = Blake2b-224(SHA3-256(addr_spec))
//! inner         = array(3)[bstr(28)(root), attributes, u8(0)]
//! wire          = array(2)[tag(24, bstr(inner)), crc32_u32]
//! address       = base58(wire)
//! ```

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use std::path::PathBuf;

/// Byron key operations.
#[derive(Args, Debug)]
pub struct ByronKeyCmd {
    #[command(subcommand)]
    command: ByronKeySubcommand,
}

#[derive(Subcommand, Debug)]
enum ByronKeySubcommand {
    /// Derive the Byron base58 address corresponding to a signing key.
    SigningKeyAddress {
        /// Path to the signing key file (text envelope)
        #[arg(long)]
        secret: PathBuf,

        /// Use Byron address format (base58)
        #[arg(long, conflicts_with = "shelley_formats")]
        byron_formats: bool,

        /// Use Shelley address format (bech32)
        #[arg(long, conflicts_with = "byron_formats")]
        shelley_formats: bool,

        /// Testnet protocol magic (omit for mainnet)
        #[arg(long)]
        testnet_magic: Option<u32>,
    },

    /// Print the public verification key corresponding to a signing key.
    SigningKeyPublic {
        /// Path to the signing key file (text envelope)
        #[arg(long)]
        secret: PathBuf,
    },

    /// Convert a Byron-legacy delegate key envelope to the current format.
    MigrateDelegateKeyFrom {
        /// Accept the old Byron-legacy envelope format
        #[arg(long, conflicts_with = "byron_formats")]
        byron_legacy_formats: bool,

        /// Accept the current Byron envelope format
        #[arg(long, conflicts_with = "byron_legacy_formats")]
        byron_formats: bool,

        /// Source signing key file (text envelope)
        #[arg(long)]
        from: PathBuf,

        /// Destination signing key file (written as the current `ByronSigningKey` type)
        #[arg(long)]
        to: PathBuf,
    },

    /// Convert a Byron signing key envelope to the Shelley Ed25519BIP32 format.
    ConvertByronKey {
        /// Path to the Byron signing key file
        #[arg(long)]
        byron_signing_key_file: PathBuf,

        /// Output path for the converted Shelley signing key file
        #[arg(long)]
        out_file: PathBuf,
    },

    /// Convert a Byron genesis verification-key envelope to the Shelley format.
    ConvertByronGenesisVkey {
        /// Path to the Byron genesis verification key file
        #[arg(long)]
        byron_genesis_vkey_file: PathBuf,

        /// Output path for the converted Shelley genesis verification key file
        #[arg(long)]
        vkey_file_out: PathBuf,
    },
}

impl ByronKeyCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            ByronKeySubcommand::SigningKeyAddress {
                secret,
                byron_formats: _,
                shelley_formats,
                testnet_magic,
            } => cmd_signing_key_address(&secret, shelley_formats, testnet_magic),

            ByronKeySubcommand::SigningKeyPublic { secret } => cmd_signing_key_public(&secret),

            ByronKeySubcommand::MigrateDelegateKeyFrom {
                byron_legacy_formats,
                byron_formats: _,
                from,
                to,
            } => cmd_migrate_delegate_key_from(&from, &to, byron_legacy_formats),

            ByronKeySubcommand::ConvertByronKey {
                byron_signing_key_file,
                out_file,
            } => cmd_convert_byron_key(&byron_signing_key_file, &out_file),

            ByronKeySubcommand::ConvertByronGenesisVkey {
                byron_genesis_vkey_file,
                vkey_file_out,
            } => cmd_convert_byron_genesis_vkey(&byron_genesis_vkey_file, &vkey_file_out),
        }
    }
}

// ---------------------------------------------------------------------------
// Command implementations
// ---------------------------------------------------------------------------

/// `signing-key-address` — derive the Byron address for a signing key.
fn cmd_signing_key_address(
    secret_path: &PathBuf,
    shelley_formats: bool,
    testnet_magic: Option<u32>,
) -> Result<()> {
    let (sk_bytes, _chain_code) = load_byron_signing_key(secret_path)
        .with_context(|| format!("{}", secret_path.display()))?;

    // Derive the Ed25519 compressed public key from the 64-byte extended scalar.
    let pubkey = extended_scalar_to_pubkey(&sk_bytes)?;
    let chain_code: [u8; 32] = _chain_code;

    if shelley_formats {
        // Shelley-format output: enterprise address bech32
        let network_tag = match testnet_magic {
            None => {
                // mainnet
                1u8
            }
            Some(_) => 0u8,
        };
        // Enterprise address: header = (0b0110 << 4) | network
        let mut addr_bytes = Vec::with_capacity(29);
        addr_bytes.push(0x60 | network_tag);
        let vk_hash = dugite_primitives::hash::blake2b_224(&pubkey);
        addr_bytes.extend_from_slice(vk_hash.as_bytes());
        let hrp = if testnet_magic.is_none() {
            "addr"
        } else {
            "addr_test"
        };
        let bech32_addr = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse(hrp)?, &addr_bytes)?;
        println!("{}", bech32_addr);
    } else {
        // Byron format: base58
        let is_mainnet = testnet_magic.is_none_or(|m| m == 764_824_073);
        let network_tag = if is_mainnet {
            None
        } else {
            let magic = testnet_magic.unwrap();
            let mut buf = Vec::new();
            minicbor::encode(magic, &mut buf)
                .map_err(|e| anyhow::anyhow!("CBOR encode magic: {}", e))?;
            Some(buf)
        };

        // Build a Byron PubKey address from the (pubkey, chain_code) pair.
        let wire_bytes = byron_pubkey_address(&pubkey, &chain_code, network_tag.as_deref())?;
        let addr_b58 = bs58::encode(&wire_bytes).into_string();
        println!("{}", addr_b58);
    }

    Ok(())
}

/// `signing-key-public` — print the hex public key from a signing key.
fn cmd_signing_key_public(secret_path: &PathBuf) -> Result<()> {
    let (sk_bytes, chain_code) = load_byron_signing_key(secret_path)
        .with_context(|| format!("{}", secret_path.display()))?;
    let pubkey = extended_scalar_to_pubkey(&sk_bytes)?;

    // cardano-cli outputs the 64-byte vkey (pubkey || chain_code) in hex
    let mut vkey64 = Vec::with_capacity(64);
    vkey64.extend_from_slice(&pubkey);
    vkey64.extend_from_slice(&chain_code);
    println!("{}", hex::encode(&vkey64));

    Ok(())
}

/// `migrate-delegate-key-from` — re-wrap a Byron (or legacy) delegate key.
fn cmd_migrate_delegate_key_from(
    from_path: &PathBuf,
    to_path: &PathBuf,
    is_legacy: bool,
) -> Result<()> {
    let content = std::fs::read_to_string(from_path)
        .with_context(|| format!("reading {}", from_path.display()))?;
    let envelope: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("parsing JSON in {}", from_path.display()))?;

    let cbor_hex = envelope["cborHex"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing cborHex in {}", from_path.display()))?;
    let cbor_bytes = hex::decode(cbor_hex)
        .with_context(|| format!("decoding cborHex in {}", from_path.display()))?;

    // Extract the raw 96-byte (or 128-byte legacy) payload.
    let raw = unwrap_cbor_bytestring(&cbor_bytes);

    let canonical_96 = if is_legacy || raw.len() == 128 {
        // Legacy format: 128 bytes; canonical = first 64 bytes || bytes 96..128.
        if raw.len() < 128 {
            bail!(
                "Expected 128-byte legacy key payload, got {} bytes in {}",
                raw.len(),
                from_path.display()
            );
        }
        let mut key = [0u8; 96];
        key[..64].copy_from_slice(&raw[..64]); // extended scalar || nonce
        key[64..].copy_from_slice(&raw[96..128]); // chain code (last 32 bytes)
        key
    } else if raw.len() == 96 {
        raw.try_into()
            .map_err(|_| anyhow::anyhow!("unexpected raw key length"))?
    } else {
        bail!(
            "Unrecognised Byron signing key payload length {} in {}",
            raw.len(),
            from_path.display()
        );
    };

    // Write out as `ByronSigningKey` with a CBOR-wrapped 96-byte payload.
    let out_envelope = serde_json::json!({
        "type": "ByronSigningKey",
        "description": "Byron Signing Key",
        "cborHex": hex::encode(cbor_wrap(&canonical_96))
    });
    std::fs::write(to_path, serde_json::to_string_pretty(&out_envelope)?)
        .with_context(|| format!("writing {}", to_path.display()))?;

    println!("Byron delegate key migrated to: {}", to_path.display());
    Ok(())
}

/// `convert-byron-key` — convert a Byron signing key to a Shelley Ed25519BIP32 envelope.
fn cmd_convert_byron_key(from_path: &PathBuf, out_path: &PathBuf) -> Result<()> {
    let (sk_extended, chain_code) =
        load_byron_signing_key(from_path).with_context(|| format!("{}", from_path.display()))?;

    // Shelley Ed25519BIP32 signing key = 96-byte CBOR bstr: sk_extended || chain_code
    let mut key96 = [0u8; 96];
    key96[..64].copy_from_slice(&sk_extended);
    key96[64..].copy_from_slice(&chain_code);

    let out_envelope = serde_json::json!({
        "type": "PaymentExtendedSigningKeyShelley_ed25519_bip32",
        "description": "Payment Signing Key",
        "cborHex": hex::encode(cbor_wrap(&key96))
    });
    std::fs::write(out_path, serde_json::to_string_pretty(&out_envelope)?)
        .with_context(|| format!("writing {}", out_path.display()))?;

    println!(
        "Converted Byron signing key written to: {}",
        out_path.display()
    );
    Ok(())
}

/// `convert-byron-genesis-vkey` — convert a Byron genesis vkey to the Shelley format.
fn cmd_convert_byron_genesis_vkey(from_path: &PathBuf, out_path: &PathBuf) -> Result<()> {
    let content = std::fs::read_to_string(from_path)
        .with_context(|| format!("reading {}", from_path.display()))?;
    let envelope: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("parsing JSON in {}", from_path.display()))?;

    let cbor_hex = envelope["cborHex"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing cborHex in {}", from_path.display()))?;
    let cbor_bytes = hex::decode(cbor_hex)
        .with_context(|| format!("decoding cborHex in {}", from_path.display()))?;

    // Byron genesis vkey CBOR payload is 64 bytes: pubkey (32) || chain_code (32).
    let raw = unwrap_cbor_bytestring(&cbor_bytes);
    if raw.len() < 32 {
        bail!(
            "Expected at least 32 bytes in Byron genesis vkey, got {} in {}",
            raw.len(),
            from_path.display()
        );
    }

    // Shelley `GenesisUTxOVerificationKey_ed25519` uses only the 32-byte pubkey
    // (not the chain code).
    let pubkey_bytes = &raw[..32];

    let out_envelope = serde_json::json!({
        "type": "GenesisUTxOVerificationKey_ed25519",
        "description": "Genesis UTxO Verification Key",
        "cborHex": hex::encode(cbor_wrap(pubkey_bytes))
    });
    std::fs::write(out_path, serde_json::to_string_pretty(&out_envelope)?)
        .with_context(|| format!("writing {}", out_path.display()))?;

    println!(
        "Converted Byron genesis vkey written to: {}",
        out_path.display()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Byron key loading helpers
// ---------------------------------------------------------------------------

/// Load a Byron signing key from a text envelope file.
///
/// Returns `(sk_extended_64, chain_code_32)`.
///
/// Accepted envelope types (in order of preference):
/// - `ByronSigningKey`         — 96-byte payload (current format)
/// - `ByronLegacySigningKey`   — 128-byte payload (cardano-sl legacy)
fn load_byron_signing_key(path: &PathBuf) -> Result<([u8; 64], [u8; 32])> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let envelope: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("parsing JSON in {}", path.display()))?;

    let type_str = envelope["type"].as_str().unwrap_or("(missing type)");
    let cbor_hex = envelope["cborHex"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing cborHex in {}", path.display()))?;
    let cbor_bytes =
        hex::decode(cbor_hex).with_context(|| format!("decoding cborHex in {}", path.display()))?;

    let raw = unwrap_cbor_bytestring(&cbor_bytes);

    match type_str {
        "ByronSigningKey" => {
            if raw.len() != 96 {
                bail!(
                    "Expected 96-byte ByronSigningKey payload, got {} in {}",
                    raw.len(),
                    path.display()
                );
            }
            let mut sk = [0u8; 64];
            let mut cc = [0u8; 32];
            sk.copy_from_slice(&raw[..64]);
            cc.copy_from_slice(&raw[64..96]);
            Ok((sk, cc))
        }
        "ByronLegacySigningKey" | "ByronSigningKeyLegacy" => {
            if raw.len() != 128 {
                bail!(
                    "Expected 128-byte ByronLegacySigningKey payload, got {} in {}",
                    raw.len(),
                    path.display()
                );
            }
            let mut sk = [0u8; 64];
            let mut cc = [0u8; 32];
            sk.copy_from_slice(&raw[..64]);
            // Legacy format: bytes 96..128 hold the chain code.
            cc.copy_from_slice(&raw[96..128]);
            Ok((sk, cc))
        }
        other => bail!(
            "Unsupported Byron signing key type '{}' in {}; expected ByronSigningKey or ByronLegacySigningKey",
            other,
            path.display()
        ),
    }
}

// ---------------------------------------------------------------------------
// Byron address construction helpers
// ---------------------------------------------------------------------------

/// Derive a compressed Ed25519 public key from a 64-byte extended scalar.
///
/// The first 32 bytes of a BIP32-Ed25519 extended key are the pre-clamped
/// Ed25519 scalar (already satisfying the BIP32-Ed25519 clamping invariant).
/// The public key = scalar × B (Ed25519 base point), compressed to 32 bytes.
///
/// We use `curve25519-dalek`'s `Scalar::from_bits_clamped` + base-point multiplication
/// rather than ed25519-dalek's `ExpandedSecretKey` (which is behind the `hazmat` feature
/// gate). This mirrors Cardano's `toXPub` → `getPublicKey` operation exactly.
fn extended_scalar_to_pubkey(sk_extended_64: &[u8; 64]) -> Result<[u8; 32]> {
    use curve25519_dalek_fork::{constants::ED25519_BASEPOINT_POINT, scalar::Scalar};

    // The scalar occupies the first 32 bytes of the extended key.
    let mut scalar_bytes = [0u8; 32];
    scalar_bytes.copy_from_slice(&sk_extended_64[..32]);

    // `from_bits` treats the input as a pre-clamped little-endian scalar
    // without reducing modulo the group order — which is exactly what BIP32-Ed25519
    // keys require (the scalar is already clamped by the BIP32 derivation, NOT by
    // the standard SHA-512 expand-and-clamp used in vanilla Ed25519).
    let scalar = Scalar::from_bits(scalar_bytes);

    // Public key = scalar × B, compressed.
    let public_point = scalar * ED25519_BASEPOINT_POINT;
    Ok(public_point.compress().to_bytes())
}

/// Build a Byron PubKey address wire-format blob.
///
/// Spending data for a PubKey address = `array(2)[i64(0), bstr(64)(pubkey||chain_code)]`
/// (variant index 0 = PubKey).
fn byron_pubkey_address(
    pubkey: &[u8; 32],
    chain_code: &[u8; 32],
    network_tag_cbor: Option<&[u8]>,
) -> Result<Vec<u8>> {
    use blake2b_simd::Params as Blake2bParams;
    use sha3::{Digest, Sha3_256};

    // Build spending data: array(2)[i64(0), bstr(64)]
    let mut vkey64 = [0u8; 64];
    vkey64[..32].copy_from_slice(pubkey);
    vkey64[32..].copy_from_slice(chain_code);
    let spending = {
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.array(2)
            .map_err(|e| anyhow::anyhow!("CBOR: {}", e))?
            .i64(0)
            .map_err(|e| anyhow::anyhow!("CBOR: {}", e))?
            .bytes(&vkey64)
            .map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
        buf
    };

    // Build attributes CBOR
    let attrs_cbor = {
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        match network_tag_cbor {
            None => {
                e.map(0).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
            }
            Some(tag) => {
                e.map(1).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
                e.u8(2).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
                e.bytes(tag).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
            }
        }
        buf
    };

    // addr_spec = array(3)[u8(0=PubKey), spending_cbor, attrs_cbor]
    let addr_spec = {
        let mut buf = Vec::new();
        buf.push(0x83u8); // array(3) header
        buf.push(0x00u8); // addr_type = 0 (PubKey), CBOR uint small
        buf.extend_from_slice(&spending);
        buf.extend_from_slice(&attrs_cbor);
        buf
    };

    // root = Blake2b-224(SHA3-256(addr_spec))
    let sha3_out: [u8; 32] = Sha3_256::digest(&addr_spec).into();
    let blake_out = Blake2bParams::new().hash_length(28).hash(&sha3_out);
    let root: [u8; 28] = blake_out.as_bytes().try_into().expect("28 bytes");

    // inner = array(3)[bstr(28)(root), attrs_cbor, u8(0)]
    let inner = {
        let mut buf = Vec::new();
        {
            let mut e = minicbor::Encoder::new(&mut buf);
            e.array(3).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
            e.bytes(&root).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
        } // drop `e` here to release borrow on `buf`
        buf.extend_from_slice(&attrs_cbor);
        let mut e2 = minicbor::Encoder::new(&mut buf);
        e2.u8(0).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
        buf
    };

    // wire = array(2)[tag(24, bstr(inner)), crc32]
    let crc = crc32fast::hash(&inner);
    let wire = {
        let mut buf = Vec::new();
        let mut e = minicbor::Encoder::new(&mut buf);
        e.array(2).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
        e.tag(minicbor::data::Tag::new(24))
            .map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
        e.bytes(&inner)
            .map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
        e.u32(crc).map_err(|e| anyhow::anyhow!("CBOR: {}", e))?;
        buf
    };

    Ok(wire)
}

// ---------------------------------------------------------------------------
// CBOR helpers
// ---------------------------------------------------------------------------

/// Unwrap a CBOR byte string header, returning the payload bytes.
/// If the data doesn't start with a recognized CBOR byte string prefix, returns
/// it unchanged (defensive: some old tooling omits the wrapper).
///
/// CBOR major type 2 (byte string) headers:
///   0x40..0x57  — tiny (length 0-23 in low 5 bits), 1-byte header
///   0x58 LL     — 1-byte length (0-255), 2-byte header
///   0x59 HH LL  — 2-byte big-endian length, 3-byte header
///   0x5a ..     — 4-byte big-endian length, 5-byte header
fn unwrap_cbor_bytestring(data: &[u8]) -> &[u8] {
    if data.is_empty() {
        return data;
    }
    match data[0] {
        // tiny (0x40..0x57): length encoded in low 5 bits — must NOT include 0x58+
        b @ 0x40..=0x57 if data.len() > 1 => {
            let payload_len = (b & 0x1f) as usize;
            if payload_len < data.len() {
                &data[1..]
            } else {
                data
            }
        }
        // 0x58 LL — 1-byte length
        0x58 if data.len() > 2 => &data[2..],
        // 0x59 HH LL — 2-byte length
        0x59 if data.len() > 3 => &data[3..],
        // 0x5a HH HH LL LL — 4-byte length
        0x5a if data.len() > 5 => &data[5..],
        _ => data,
    }
}

/// Wrap bytes in a CBOR byte string (major type 2).
fn cbor_wrap(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let len = data.len();
    if len < 24 {
        out.push(0x40 | len as u8);
    } else if len < 256 {
        out.push(0x58);
        out.push(len as u8);
    } else {
        out.push(0x59);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    }
    out.extend_from_slice(data);
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    // -----------------------------------------------------------------------
    // Helper: write a synthetic ByronSigningKey envelope to a tempfile.
    // -----------------------------------------------------------------------

    fn write_byron_sk_envelope(key96: &[u8; 96]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        let env = serde_json::json!({
            "type": "ByronSigningKey",
            "description": "Byron Signing Key",
            "cborHex": hex::encode(cbor_wrap(key96))
        });
        write!(f, "{}", serde_json::to_string_pretty(&env).unwrap()).unwrap();
        f
    }

    fn write_byron_legacy_sk_envelope(key128: &[u8; 128]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        let env = serde_json::json!({
            "type": "ByronLegacySigningKey",
            "description": "Byron Legacy Signing Key",
            "cborHex": hex::encode(cbor_wrap(key128))
        });
        write!(f, "{}", serde_json::to_string_pretty(&env).unwrap()).unwrap();
        f
    }

    fn write_byron_genesis_vkey_envelope(vkey64: &[u8; 64]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        let env = serde_json::json!({
            "type": "ByronGenesisVerificationKey",
            "description": "Byron Genesis Verification Key",
            "cborHex": hex::encode(cbor_wrap(vkey64))
        });
        write!(f, "{}", serde_json::to_string_pretty(&env).unwrap()).unwrap();
        f
    }

    // -----------------------------------------------------------------------
    // Unit tests: cbor_wrap / unwrap_cbor_bytestring round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn test_cbor_wrap_tiny() {
        let data = [0xABu8; 10];
        let wrapped = cbor_wrap(&data);
        assert_eq!(wrapped[0], 0x4A); // 0x40 | 10
        let unwrapped = unwrap_cbor_bytestring(&wrapped);
        assert_eq!(unwrapped, data);
    }

    #[test]
    fn test_cbor_wrap_medium() {
        let data = vec![0xCDu8; 96];
        let wrapped = cbor_wrap(&data);
        assert_eq!(wrapped[0], 0x58); // 1-byte length prefix
        assert_eq!(wrapped[1], 96);
        let unwrapped = unwrap_cbor_bytestring(&wrapped);
        assert_eq!(unwrapped, data.as_slice());
    }

    // -----------------------------------------------------------------------
    // load_byron_signing_key: round-trip ByronSigningKey envelope
    // -----------------------------------------------------------------------

    #[test]
    fn test_load_byron_signing_key_96_bytes() {
        // Construct a synthetic 96-byte key where each region has a distinct byte value.
        let mut key96 = [0u8; 96];
        // Clamp the scalar correctly (BIP32-Ed25519 clamping):
        // bit 0,1,2 of byte 0 cleared; bit 7 of byte 31 cleared; bit 6 of byte 31 set.
        key96[0] = 0xF8; // low 3 bits cleared
        key96[31] = 0x40; // bit 7 clear, bit 6 set
        for b in key96[32..64].iter_mut() {
            *b = 0xBB; // nonce half of extended key
        }
        for b in key96[64..96].iter_mut() {
            *b = 0xCC; // chain code
        }

        let f = write_byron_sk_envelope(&key96);
        let (sk, cc) = load_byron_signing_key(&f.path().to_path_buf()).unwrap();

        assert_eq!(sk, key96[..64]);
        assert_eq!(cc, key96[64..96]);
    }

    #[test]
    fn test_load_byron_legacy_signing_key_128_bytes() {
        let mut key128 = [0u8; 128];
        // Fill with distinct patterns
        key128[0] = 0xF8;
        key128[31] = 0x40;
        for b in key128[32..64].iter_mut() {
            *b = 0xBB;
        }
        // Bytes 64..96: pubkey (public key in legacy format)
        for b in key128[64..96].iter_mut() {
            *b = 0x11;
        }
        // Bytes 96..128: chain code
        for b in key128[96..128].iter_mut() {
            *b = 0xCC;
        }

        let f = write_byron_legacy_sk_envelope(&key128);
        let (sk, cc) = load_byron_signing_key(&f.path().to_path_buf()).unwrap();

        // Extended scalar = bytes 0..64
        assert_eq!(&sk[..], &key128[..64]);
        // Chain code = bytes 96..128
        assert_eq!(&cc[..], &key128[96..128]);
    }

    // -----------------------------------------------------------------------
    // convert-byron-key: round-trip through file
    // -----------------------------------------------------------------------

    #[test]
    fn test_convert_byron_key_round_trip() {
        let mut key96 = [0u8; 96];
        key96[0] = 0xF8;
        key96[31] = 0x40;
        for (i, b) in key96[32..].iter_mut().enumerate() {
            *b = (32 + i) as u8;
        }

        let src = write_byron_sk_envelope(&key96);
        let dst = NamedTempFile::new().unwrap();

        cmd_convert_byron_key(&src.path().to_path_buf(), &dst.path().to_path_buf()).unwrap();

        // Read back and verify
        let content = std::fs::read_to_string(dst.path()).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            env["type"].as_str().unwrap(),
            "PaymentExtendedSigningKeyShelley_ed25519_bip32"
        );

        let cbor_hex = env["cborHex"].as_str().unwrap();
        let cbor_bytes = hex::decode(cbor_hex).unwrap();
        let raw = unwrap_cbor_bytestring(&cbor_bytes);
        assert_eq!(raw.len(), 96);
        assert_eq!(raw, &key96[..]);
    }

    // -----------------------------------------------------------------------
    // convert-byron-genesis-vkey: extracts first 32 bytes as pubkey
    // -----------------------------------------------------------------------

    #[test]
    fn test_convert_byron_genesis_vkey() {
        let mut vkey64 = [0u8; 64];
        for (i, b) in vkey64.iter_mut().enumerate() {
            *b = i as u8;
        }

        let src = write_byron_genesis_vkey_envelope(&vkey64);
        let dst = NamedTempFile::new().unwrap();

        cmd_convert_byron_genesis_vkey(&src.path().to_path_buf(), &dst.path().to_path_buf())
            .unwrap();

        let content = std::fs::read_to_string(dst.path()).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            env["type"].as_str().unwrap(),
            "GenesisUTxOVerificationKey_ed25519"
        );

        let cbor_hex = env["cborHex"].as_str().unwrap();
        let cbor_bytes = hex::decode(cbor_hex).unwrap();
        let raw = unwrap_cbor_bytestring(&cbor_bytes);
        // Only the 32-byte pubkey, not the chain code
        assert_eq!(raw.len(), 32);
        assert_eq!(raw, &vkey64[..32]);
    }

    // -----------------------------------------------------------------------
    // migrate-delegate-key-from: normalise legacy format
    // -----------------------------------------------------------------------

    #[test]
    fn test_migrate_delegate_key_from_legacy() {
        let mut key128 = [0u8; 128];
        key128[0] = 0xF8;
        key128[31] = 0x40;
        for b in key128[32..64].iter_mut() {
            *b = 0xBB;
        }
        for b in key128[64..96].iter_mut() {
            *b = 0x11; // pubkey in legacy layout
        }
        for b in key128[96..128].iter_mut() {
            *b = 0xCC; // chain code
        }

        let src = write_byron_legacy_sk_envelope(&key128);
        let dst = NamedTempFile::new().unwrap();

        cmd_migrate_delegate_key_from(
            &src.path().to_path_buf(),
            &dst.path().to_path_buf(),
            true, // is_legacy
        )
        .unwrap();

        let content = std::fs::read_to_string(dst.path()).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(env["type"].as_str().unwrap(), "ByronSigningKey");

        let raw = unwrap_cbor_bytestring(&hex::decode(env["cborHex"].as_str().unwrap()).unwrap())
            .to_vec();
        assert_eq!(raw.len(), 96);
        // Extended scalar = bytes 0..64 of original
        assert_eq!(&raw[..64], &key128[..64]);
        // Chain code = bytes 96..128 of original
        assert_eq!(&raw[64..], &key128[96..128]);
    }

    // -----------------------------------------------------------------------
    // migrate-delegate-key-from: current format (96 bytes) pass-through
    // -----------------------------------------------------------------------

    #[test]
    fn test_migrate_delegate_key_from_current_format() {
        let mut key96 = [0u8; 96];
        key96[0] = 0xF8;
        key96[31] = 0x40;
        for (i, b) in key96[32..].iter_mut().enumerate() {
            *b = ((32 + i) * 2) as u8;
        }

        let src = write_byron_sk_envelope(&key96);
        let dst = NamedTempFile::new().unwrap();

        cmd_migrate_delegate_key_from(
            &src.path().to_path_buf(),
            &dst.path().to_path_buf(),
            false, // not legacy — parse from actual type
        )
        .unwrap();

        let content = std::fs::read_to_string(dst.path()).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(env["type"].as_str().unwrap(), "ByronSigningKey");

        let raw = unwrap_cbor_bytestring(&hex::decode(env["cborHex"].as_str().unwrap()).unwrap())
            .to_vec();
        assert_eq!(raw.len(), 96);
        assert_eq!(&raw[..], &key96[..]);
    }

    // -----------------------------------------------------------------------
    // Byron PubKey address construction: structural invariants
    // -----------------------------------------------------------------------

    #[test]
    fn test_byron_pubkey_address_mainnet_base58_non_empty() {
        let pubkey = [0xABu8; 32];
        let chain_code = [0xCDu8; 32];
        let wire = byron_pubkey_address(&pubkey, &chain_code, None).unwrap();
        // Base58 of a valid Byron address is always non-empty and starts with a known prefix.
        let b58 = bs58::encode(&wire).into_string();
        assert!(!b58.is_empty());
        // Wire is array(2) so starts with 0x82.
        assert_eq!(wire[0], 0x82);
    }

    #[test]
    fn test_byron_pubkey_address_testnet_differs_from_mainnet() {
        let pubkey = [0x11u8; 32];
        let chain_code = [0x22u8; 32];

        let wire_main = byron_pubkey_address(&pubkey, &chain_code, None).unwrap();

        let mut magic_cbor = Vec::new();
        minicbor::encode(2u32, &mut magic_cbor).unwrap();
        let wire_test = byron_pubkey_address(&pubkey, &chain_code, Some(&magic_cbor)).unwrap();

        // Testnet address must differ from mainnet due to network tag attribute.
        assert_ne!(wire_main, wire_test);
    }

    // -----------------------------------------------------------------------
    // signing-key-public: pubkey extraction is deterministic
    // -----------------------------------------------------------------------

    #[test]
    fn test_signing_key_public_deterministic() {
        // Use two distinct keys with non-trivial scalar bytes and verify that
        // (a) derived pubkeys are non-zero, (b) distinct scalars → distinct pubkeys.
        //
        // BIP32-Ed25519 clamping rules:
        //   byte  0: bits 0,1,2 cleared  (& 0xF8)
        //   byte 31: bit 7 cleared (& 0x7F), bit 6 set (| 0x40)
        let mut key96_a = [0u8; 96];
        let mut key96_b = [0u8; 96];

        // Fill scalars with non-zero patterns so we get non-trivial EC points.
        for (i, b) in key96_a[..32].iter_mut().enumerate() {
            *b = (0xAA + i as u8).wrapping_mul(3);
        }
        key96_a[0] &= 0xF8;
        key96_a[31] = (key96_a[31] & 0x7F) | 0x40;

        for (i, b) in key96_b[..32].iter_mut().enumerate() {
            *b = (0xBB + i as u8).wrapping_mul(5);
        }
        key96_b[0] &= 0xF8;
        key96_b[31] = (key96_b[31] & 0x7F) | 0x40;

        // Give each key a distinct chain code too.
        for b in key96_a[64..96].iter_mut() {
            *b = 0xAA;
        }
        for b in key96_b[64..96].iter_mut() {
            *b = 0xBB;
        }

        let fa = write_byron_sk_envelope(&key96_a);
        let fb = write_byron_sk_envelope(&key96_b);

        let (sk_a, cc_a) = load_byron_signing_key(&fa.path().to_path_buf()).unwrap();
        let (sk_b, cc_b) = load_byron_signing_key(&fb.path().to_path_buf()).unwrap();

        let pk_a = extended_scalar_to_pubkey(&sk_a).unwrap();
        let pk_b = extended_scalar_to_pubkey(&sk_b).unwrap();

        // Keys must produce non-trivial pubkeys (not all-zero).
        assert_ne!(pk_a, [0u8; 32], "pk_a must be non-zero");
        assert_ne!(pk_b, [0u8; 32], "pk_b must be non-zero");
        // Distinct scalars → distinct public keys.
        assert_ne!(pk_a, pk_b);
        // Chain codes are independent of pubkey derivation.
        assert_ne!(cc_a, cc_b);
    }
}
