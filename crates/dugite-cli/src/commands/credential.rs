//! Shared credential/key-selector resolution helpers.
//!
//! cardano-cli 11 repeats a handful of selector shapes across many
//! subcommands: "give me a stake credential" (verification key / file / hash
//! / script hash / script file / bech32 address), "give me a pool ID"
//! (verification key / extended verification key / cold key file / bech32 or
//! hex pool id), and "give me a DRep" (script hash / verification key / file
//! / key hash / always-abstain / always-no-confidence). Centralising the
//! resolution here means every new #1008 shim (stake-address combined certs,
//! governance committee certs, MIR certs) decodes these the same way instead
//! of re-deriving five ad-hoc branches per command.

use anyhow::Result;
use clap::Args;
use std::path::{Path, PathBuf};

/// Decode `s` as hex if every character is a hex digit and the length is
/// even; otherwise fall back to bech32. Mirrors the `starts_with("pool")`
/// heuristic already used ad hoc in `stake_address.rs`/`query.rs`, made
/// general: cardano-cli documents most of these `STRING` selectors as
/// "Bech32 or hex-encoded".
pub(crate) fn decode_hex_or_bech32(s: &str) -> Result<Vec<u8>> {
    let s = s.trim();
    let looks_hex =
        !s.is_empty() && s.len().is_multiple_of(2) && s.bytes().all(|b| b.is_ascii_hexdigit());
    if looks_hex {
        if let Ok(bytes) = hex::decode(s) {
            return Ok(bytes);
        }
    }
    let (_, bytes) =
        bech32::decode(s).map_err(|e| anyhow::anyhow!("'{s}' is neither hex nor bech32: {e}"))?;
    Ok(bytes)
}

/// Strip a CBOR bytestring header (`0x58 <len>` or short-form `0x4N`) off a
/// text-envelope `cborHex` payload, returning the raw key bytes.
fn strip_cbor_bstr_header(cbor_bytes: &[u8]) -> &[u8] {
    if cbor_bytes.len() > 2 && cbor_bytes[0] == 0x58 {
        &cbor_bytes[2..]
    } else if cbor_bytes.len() > 1 && (cbor_bytes[0] & 0xe0) == 0x40 {
        &cbor_bytes[1..]
    } else {
        cbor_bytes
    }
}

/// Load the raw verification-key bytes out of a text-envelope JSON file.
pub(crate) fn load_vkey_bytes_from_envelope(path: &Path) -> Result<Vec<u8>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", path.display()))?;
    let env: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("'{}' is not valid JSON: {e}", path.display()))?;
    let cbor_hex = env
        .get("cborHex")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing cborHex in {}", path.display()))?;
    let cbor_bytes = hex::decode(cbor_hex.trim())
        .map_err(|e| anyhow::anyhow!("bad cborHex in {}: {e}", path.display()))?;
    Ok(strip_cbor_bstr_header(&cbor_bytes).to_vec())
}

/// Blake2b-224 hash of a verification key loaded from a text-envelope file.
/// If the envelope carries an extended key (64 bytes: 32-byte pubkey +
/// 32-byte chain code), only the leading 32-byte pubkey is hashed.
pub(crate) fn load_vkey_hash_from_envelope(path: &Path) -> Result<Vec<u8>> {
    let bytes = load_vkey_bytes_from_envelope(path)?;
    Ok(vkey_bytes_to_hash(&bytes))
}

/// Blake2b-224 hash of raw verification-key bytes. A 64-byte extended key
/// (32-byte pubkey || 32-byte chain code) is truncated to its pubkey half
/// first — matches cardano-api's `Api.Key.verificationKeyHash` for extended
/// keys, which always hashes the non-extended 32-byte public key.
pub(crate) fn vkey_bytes_to_hash(bytes: &[u8]) -> Vec<u8> {
    let key = if bytes.len() == 64 {
        &bytes[..32]
    } else {
        bytes
    };
    dugite_primitives::hash::blake2b_224(key)
        .as_bytes()
        .to_vec()
}

/// Resolve a `STRING` verification-key argument (bech32 or hex) to its
/// Blake2b-224 hash, truncating a 64-byte extended key to its pubkey half.
pub(crate) fn vkey_string_to_hash(s: &str) -> Result<Vec<u8>> {
    let bytes = decode_hex_or_bech32(s)?;
    Ok(vkey_bytes_to_hash(&bytes))
}

/// A resolved credential: key-hash or script-hash, 28 bytes either way.
/// `cred_type` follows the Shelley/Conway `stake_credential` discriminator:
/// 0 = key hash, 1 = script hash.
pub(crate) struct ResolvedCredential {
    pub cred_type: u8,
    pub hash: Vec<u8>,
}

/// Resolve the common 5-way selector: `--*-verification-key STRING`,
/// `--*-verification-key-file FILEPATH`, a key-hash flag (name given by
/// `key_hash_flag`, since cardano-cli spells it `--*-key-hash` in some
/// families and `--*-verification-key-hash` in others), `--*-script-hash
/// HASH`, `--*-script-file FILEPATH`. Exactly one of the five `Option`s must
/// be `Some`; `label` names the flag family in the remaining error messages
/// (e.g. `"--stake-"`, `"--cold-"`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_credential_5way(
    verification_key: Option<&str>,
    verification_key_file: Option<&Path>,
    key_hash: Option<&str>,
    script_hash: Option<&str>,
    script_file: Option<&Path>,
    label: &str,
) -> Result<ResolvedCredential> {
    resolve_credential_5way_named(
        verification_key,
        verification_key_file,
        key_hash,
        script_hash,
        script_file,
        label,
        &format!("{label}key-hash"),
    )
}

/// As [`resolve_credential_5way`], but with an explicit flag name for the
/// key-hash branch's error text (cardano-cli spells this `--*-key-hash` for
/// stake/DRep credentials and `--*-verification-key-hash` for Constitutional
/// Committee cold/hot credentials — see `CcColdArgs`/`CcHotArgs`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_credential_5way_named(
    verification_key: Option<&str>,
    verification_key_file: Option<&Path>,
    key_hash: Option<&str>,
    script_hash: Option<&str>,
    script_file: Option<&Path>,
    label: &str,
    key_hash_flag: &str,
) -> Result<ResolvedCredential> {
    if let Some(vk) = verification_key {
        return Ok(ResolvedCredential {
            cred_type: 0,
            hash: vkey_string_to_hash(vk)?,
        });
    }
    if let Some(path) = verification_key_file {
        return Ok(ResolvedCredential {
            cred_type: 0,
            hash: load_vkey_hash_from_envelope(path)?,
        });
    }
    if let Some(h) = key_hash {
        let bytes = decode_hex_or_bech32(h)?;
        if bytes.len() != 28 {
            anyhow::bail!("{key_hash_flag} must be 28 bytes, got {}", bytes.len());
        }
        return Ok(ResolvedCredential {
            cred_type: 0,
            hash: bytes,
        });
    }
    if let Some(h) = script_hash {
        let bytes = decode_hex_or_bech32(h)?;
        if bytes.len() != 28 {
            anyhow::bail!("{label}script-hash must be 28 bytes, got {}", bytes.len());
        }
        return Ok(ResolvedCredential {
            cred_type: 1,
            hash: bytes,
        });
    }
    if let Some(path) = script_file {
        let hash = crate::commands::hash::hash_script_file(path)?;
        return Ok(ResolvedCredential {
            cred_type: 1,
            hash: hash.as_bytes().to_vec(),
        });
    }
    anyhow::bail!(
        "missing selector: pass one of {label}verification-key, {label}verification-key-file, \
         {key_hash_flag}, {label}script-hash, or {label}script-file"
    );
}

/// Extract `(cred_type, hash28)` from a bech32 stake (reward) address.
/// Header byte bit 4 (`0x10`) selects key (0) vs script (1) credential —
/// same convention as `dugite-ledger`'s `reward_account_to_hash`.
pub(crate) fn stake_address_to_credential(addr: &str) -> Result<ResolvedCredential> {
    let (_, bytes) =
        bech32::decode(addr).map_err(|e| anyhow::anyhow!("invalid bech32 address: {e}"))?;
    if bytes.len() != 29 {
        anyhow::bail!(
            "'{addr}' is not a 29-byte stake address ({} bytes)",
            bytes.len()
        );
    }
    let cred_type = if bytes[0] & 0x10 != 0 { 1 } else { 0 };
    Ok(ResolvedCredential {
        cred_type,
        hash: bytes[1..].to_vec(),
    })
}

/// Resolve a stake pool ID from cardano-cli's 4-way selector:
/// `--stake-pool-verification-key`, `--stake-pool-verification-extended-key`,
/// `--cold-verification-key-file`, `--stake-pool-id` (bech32 `pool1…` or hex).
/// Always a 28-byte key hash — pools have no script variant.
pub(crate) fn resolve_pool_id(
    verification_key: Option<&str>,
    verification_extended_key: Option<&str>,
    cold_verification_key_file: Option<&Path>,
    stake_pool_id: Option<&str>,
) -> Result<Vec<u8>> {
    if let Some(vk) = verification_key {
        return vkey_string_to_hash(vk);
    }
    if let Some(vk) = verification_extended_key {
        return vkey_string_to_hash(vk);
    }
    if let Some(path) = cold_verification_key_file {
        return load_vkey_hash_from_envelope(path);
    }
    if let Some(id) = stake_pool_id {
        let bytes = decode_hex_or_bech32(id)?;
        if bytes.len() != 28 {
            anyhow::bail!("--stake-pool-id must be 28 bytes, got {}", bytes.len());
        }
        return Ok(bytes);
    }
    anyhow::bail!(
        "missing selector: pass one of --stake-pool-verification-key, \
         --stake-pool-verification-extended-key, --cold-verification-key-file, \
         or --stake-pool-id"
    );
}

/// A resolved DRep target for a vote-delegation/vote-cert CBOR `drep` arm.
pub(crate) enum DRepSelector {
    Key(Vec<u8>),
    Script(Vec<u8>),
    AlwaysAbstain,
    AlwaysNoConfidence,
}

impl DRepSelector {
    /// Encode the `drep` CBOR arm: `[0, hash]` key / `[1, hash]` script /
    /// `[2]` abstain / `[3]` no-confidence.
    pub(crate) fn encode(&self, enc: &mut minicbor::Encoder<&mut Vec<u8>>) -> Result<()> {
        match self {
            DRepSelector::Key(h) => {
                enc.array(2)?;
                enc.u32(0)?;
                enc.bytes(h)?;
            }
            DRepSelector::Script(h) => {
                enc.array(2)?;
                enc.u32(1)?;
                enc.bytes(h)?;
            }
            DRepSelector::AlwaysAbstain => {
                enc.array(1)?;
                enc.u32(2)?;
            }
            DRepSelector::AlwaysNoConfidence => {
                enc.array(1)?;
                enc.u32(3)?;
            }
        }
        Ok(())
    }
}

/// Resolve cardano-cli's DRep selector: `--drep-script-hash`,
/// `--drep-verification-key`, `--drep-verification-key-file`,
/// `--drep-key-hash`, `--always-abstain`, `--always-no-confidence`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn resolve_drep_selector(
    drep_script_hash: Option<&str>,
    drep_verification_key: Option<&str>,
    drep_verification_key_file: Option<&Path>,
    drep_key_hash: Option<&str>,
    always_abstain: bool,
    always_no_confidence: bool,
) -> Result<DRepSelector> {
    if always_abstain {
        return Ok(DRepSelector::AlwaysAbstain);
    }
    if always_no_confidence {
        return Ok(DRepSelector::AlwaysNoConfidence);
    }
    if let Some(h) = drep_script_hash {
        let bytes = decode_hex_or_bech32(h)?;
        if bytes.len() != 28 {
            anyhow::bail!("--drep-script-hash must be 28 bytes, got {}", bytes.len());
        }
        return Ok(DRepSelector::Script(bytes));
    }
    if let Some(vk) = drep_verification_key {
        return Ok(DRepSelector::Key(vkey_string_to_hash(vk)?));
    }
    if let Some(path) = drep_verification_key_file {
        return Ok(DRepSelector::Key(load_vkey_hash_from_envelope(path)?));
    }
    if let Some(h) = drep_key_hash {
        let bytes = decode_hex_or_bech32(h)?;
        if bytes.len() != 28 {
            anyhow::bail!("--drep-key-hash must be 28 bytes, got {}", bytes.len());
        }
        return Ok(DRepSelector::Key(bytes));
    }
    anyhow::bail!(
        "missing DRep selector: pass --drep-script-hash, --drep-verification-key, \
         --drep-verification-key-file, --drep-key-hash, --always-abstain, \
         or --always-no-confidence"
    );
}

/// The 5-way Constitutional Committee COLD credential selector cardano-cli
/// repeats across `governance committee create-cold-key-resignation-certificate`,
/// `governance committee create-hot-key-authorization-certificate`, and
/// `governance vote create`'s CC-voter arm.
#[derive(Args, Debug)]
pub(crate) struct CcColdArgs {
    #[arg(long = "cold-verification-key", value_name = "STRING")]
    pub(crate) cold_verification_key: Option<String>,
    #[arg(long = "cold-verification-key-file", value_name = "FILEPATH")]
    pub(crate) cold_verification_key_file: Option<PathBuf>,
    #[arg(long = "cold-verification-key-hash", value_name = "STRING")]
    pub(crate) cold_verification_key_hash: Option<String>,
    #[arg(long = "cold-script-hash", value_name = "HASH")]
    pub(crate) cold_script_hash: Option<String>,
    #[arg(long = "cold-script-file", value_name = "FILEPATH")]
    pub(crate) cold_script_file: Option<PathBuf>,
}

impl CcColdArgs {
    pub(crate) fn resolve(&self) -> Result<ResolvedCredential> {
        resolve_credential_5way_named(
            self.cold_verification_key.as_deref(),
            self.cold_verification_key_file.as_deref(),
            self.cold_verification_key_hash.as_deref(),
            self.cold_script_hash.as_deref(),
            self.cold_script_file.as_deref(),
            "--cold-",
            "--cold-verification-key-hash",
        )
    }
}

/// The 5-way Constitutional Committee HOT credential selector, mirroring
/// [`CcColdArgs`] with the `--hot-*` flag family.
#[derive(Args, Debug)]
pub(crate) struct CcHotArgs {
    #[arg(long = "hot-verification-key", value_name = "STRING")]
    pub(crate) hot_verification_key: Option<String>,
    #[arg(long = "hot-verification-key-file", value_name = "FILEPATH")]
    pub(crate) hot_verification_key_file: Option<PathBuf>,
    #[arg(long = "hot-verification-key-hash", value_name = "STRING")]
    pub(crate) hot_verification_key_hash: Option<String>,
    #[arg(long = "hot-script-hash", value_name = "HASH")]
    pub(crate) hot_script_hash: Option<String>,
    #[arg(long = "hot-script-file", value_name = "FILEPATH")]
    pub(crate) hot_script_file: Option<PathBuf>,
}

impl CcHotArgs {
    pub(crate) fn resolve(&self) -> Result<ResolvedCredential> {
        resolve_credential_5way_named(
            self.hot_verification_key.as_deref(),
            self.hot_verification_key_file.as_deref(),
            self.hot_verification_key_hash.as_deref(),
            self.hot_script_hash.as_deref(),
            self.hot_script_file.as_deref(),
            "--hot-",
            "--hot-verification-key-hash",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_hex_or_bech32_hex() {
        let bytes = decode_hex_or_bech32("deadbeef").unwrap();
        assert_eq!(bytes, vec![0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn test_decode_hex_or_bech32_bech32() {
        let hrp = bech32::Hrp::parse("pool").unwrap();
        let encoded = bech32::encode::<bech32::Bech32>(hrp, &[0xab; 28]).unwrap();
        let bytes = decode_hex_or_bech32(&encoded).unwrap();
        assert_eq!(bytes, vec![0xab; 28]);
    }

    #[test]
    fn test_vkey_bytes_to_hash_truncates_extended_key() {
        let mut extended = vec![0xcdu8; 32];
        extended.extend_from_slice(&[0xefu8; 32]);
        let truncated_hash = vkey_bytes_to_hash(&extended);
        let plain_hash = vkey_bytes_to_hash(&[0xcdu8; 32]);
        assert_eq!(truncated_hash, plain_hash);
    }

    #[test]
    fn test_stake_address_to_credential_key_vs_script() {
        // Testnet key-hash reward address: header 0xe0.
        let mut key_addr = vec![0xe0u8];
        key_addr.extend_from_slice(&[0x11; 28]);
        let hrp = bech32::Hrp::parse("stake_test").unwrap();
        let encoded = bech32::encode::<bech32::Bech32>(hrp, &key_addr).unwrap();
        let resolved = stake_address_to_credential(&encoded).unwrap();
        assert_eq!(resolved.cred_type, 0);
        assert_eq!(resolved.hash, vec![0x11; 28]);

        // Testnet script-hash reward address: header 0xf0.
        let mut script_addr = vec![0xf0u8];
        script_addr.extend_from_slice(&[0x22; 28]);
        let encoded = bech32::encode::<bech32::Bech32>(hrp, &script_addr).unwrap();
        let resolved = stake_address_to_credential(&encoded).unwrap();
        assert_eq!(resolved.cred_type, 1);
        assert_eq!(resolved.hash, vec![0x22; 28]);
    }

    #[test]
    fn test_resolve_drep_selector_abstain_and_no_confidence() {
        let sel = resolve_drep_selector(None, None, None, None, true, false).unwrap();
        assert!(matches!(sel, DRepSelector::AlwaysAbstain));
        let sel = resolve_drep_selector(None, None, None, None, false, true).unwrap();
        assert!(matches!(sel, DRepSelector::AlwaysNoConfidence));
    }

    #[test]
    fn test_resolve_drep_selector_key_hash() {
        let hash_hex = hex::encode([0x33u8; 28]);
        let sel = resolve_drep_selector(None, None, None, Some(&hash_hex), false, false).unwrap();
        match sel {
            DRepSelector::Key(h) => assert_eq!(h, vec![0x33; 28]),
            _ => panic!("expected Key"),
        }
    }

    #[test]
    fn test_resolve_pool_id_hex_and_bech32() {
        let hex_id = hex::encode([0x44u8; 28]);
        let bytes = resolve_pool_id(None, None, None, Some(&hex_id)).unwrap();
        assert_eq!(bytes, vec![0x44; 28]);

        let hrp = bech32::Hrp::parse("pool").unwrap();
        let encoded = bech32::encode::<bech32::Bech32>(hrp, &[0x55; 28]).unwrap();
        let bytes = resolve_pool_id(None, None, None, Some(&encoded)).unwrap();
        assert_eq!(bytes, vec![0x55; 28]);
    }
}
