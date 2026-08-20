//! `dugite-cli key` — key utilities.
//!
//! # Relationship to cardano-cli (#935 item 4)
//!
//! This whole subcommand group is a **deliberate dugite extension**;
//! cardano-cli 11 has no `key generate-payment-key`,
//! `key generate-stake-key`, or `key verification-key-hash` counterpart. The
//! equivalent cardano-cli workflows are:
//!
//! | dugite                          | cardano-cli equivalent                          |
//! |---------------------------------|-------------------------------------------------|
//! | `key generate-payment-key`      | `address key-gen`                               |
//! | `key generate-stake-key`        | `stake-address key-gen`                         |
//! | `key verification-key-hash`     | `address key-hash` / `stake-address key-hash`   |
//!
//! The cardano-cli spellings are all implemented too, so no script written
//! against cardano-cli needs these. They are kept because they are convenient
//! and already in use; they are additive and can never change the behaviour of
//! a cardano-cli-compatible invocation.

use crate::commands::credential::load_vkey_bytes_from_envelope;
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use dugite_crypto::keys::{PaymentSigningKey, TextEnvelope};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct KeyCmd {
    #[command(subcommand)]
    command: KeySubcommand,
}

/// Envelope `type` values `key verification-key-hash` accepts: the
/// non-extended Ed25519 verification-key types dugite-cli itself writes,
/// plus the cardano-cli conventional ones with the same 32-byte payload and
/// blake2b-224 hash. Signing keys are NEVER accepted, and neither are
/// KES/VRF verification keys (VRF key hashes are blake2b-256, printed by
/// `node key-hash-VRF`).
const ACCEPTED_VKEY_TYPES: [&str; 9] = [
    "PaymentVerificationKeyShelley_ed25519",
    "StakeVerificationKeyShelley_ed25519",
    "StakePoolVerificationKey_ed25519",
    "GenesisVerificationKey_ed25519",
    "GenesisDelegateVerificationKey_ed25519",
    "GenesisUTxOVerificationKey_ed25519",
    "DRepVerificationKey_ed25519",
    "ConstitutionalCommitteeColdVerificationKey_ed25519",
    "ConstitutionalCommitteeHotVerificationKey_ed25519",
];

#[derive(Subcommand, Debug)]
enum KeySubcommand {
    /// Generate a payment key pair
    GeneratePaymentKey {
        /// Output path for the signing key
        #[arg(long)]
        signing_key_file: PathBuf,
        /// Output path for the verification key
        #[arg(long)]
        verification_key_file: PathBuf,
    },
    /// Generate a stake key pair
    GenerateStakeKey {
        /// Output path for the signing key
        #[arg(long)]
        signing_key_file: PathBuf,
        /// Output path for the verification key
        #[arg(long)]
        verification_key_file: PathBuf,
    },
    /// Get the verification key hash
    VerificationKeyHash {
        /// Path to the verification key file
        #[arg(long)]
        verification_key_file: PathBuf,
    },
    /// Convert a Byron payment, genesis or genesis delegate key (signing or
    /// verification) to a corresponding Shelley-format key.
    ///
    /// #1091. Verified against a real cardano-cli 11.0.0.0: this command
    /// does NOT change the key's cryptographic type or bytes — it re-wraps
    /// the SAME key material into cardano-cli's modern JSON text-envelope
    /// format, selecting the output `type` label purely from the
    /// `--byron-*-key-type` flag (no validation that the input actually IS
    /// that kind of key — confirmed empirically: feeding a payment key with
    /// `--byron-genesis-key-type` relabels it as a genesis key without
    /// complaint). "Shelley-format" in cardano-cli's own `--help` text means
    /// "the modern JSON envelope", not a different cryptographic key type —
    /// the six output `type` strings below were captured from real
    /// cardano-cli output on all three flag families x signing/verification,
    /// not derived from documentation.
    ConvertByronKey {
        #[arg(long = "byron-payment-key-type")]
        byron_payment_key_type: bool,
        #[arg(long = "legacy-byron-payment-key-type")]
        legacy_byron_payment_key_type: bool,
        #[arg(long = "byron-genesis-key-type")]
        byron_genesis_key_type: bool,
        #[arg(long = "legacy-byron-genesis-key-type")]
        legacy_byron_genesis_key_type: bool,
        #[arg(long = "byron-genesis-delegate-key-type")]
        byron_genesis_delegate_key_type: bool,
        #[arg(long = "legacy-byron-genesis-delegate-key-type")]
        legacy_byron_genesis_delegate_key_type: bool,
        #[arg(
            long = "byron-signing-key-file",
            value_name = "FILEPATH",
            conflicts_with = "byron_verification_key_file"
        )]
        byron_signing_key_file: Option<PathBuf>,
        #[arg(long = "byron-verification-key-file", value_name = "FILEPATH")]
        byron_verification_key_file: Option<PathBuf>,
        #[arg(long = "password", value_name = "TEXT")]
        password: Option<String>,
        #[arg(long = "out-file", value_name = "FILEPATH")]
        out_file: PathBuf,
    },
    /// Convert a Base64-encoded Byron genesis verification key to a Shelley
    /// genesis verification key
    ///
    /// #1091. Verified against a real cardano-cli 11.0.0.0: the input is a
    /// 64-byte Byron extended verification key (32-byte pubkey || 32-byte
    /// chain code), Base64-encoded inline (NOT a file); only the leading
    /// 32-byte pubkey survives into the output, wrapped as a plain (non-
    /// extended) `GenesisVerificationKey_ed25519` — a DIFFERENT envelope
    /// type from `byron key convert-byron-genesis-vkey`'s
    /// `GenesisUTxOVerificationKey_ed25519` (that command handles a
    /// different real-world case: AVVM/redemption genesis UTxO keys, not
    /// genesis DELEGATE keys). The two commands are not aliases of each
    /// other despite the similar names.
    ConvertByronGenesisVkey {
        #[arg(long = "byron-genesis-verification-key", value_name = "BASE64")]
        byron_genesis_verification_key: String,
        #[arg(long = "out-file", value_name = "FILEPATH")]
        out_file: PathBuf,
    },
    /// Get a verification key from a signing key. This supports all key
    /// types.
    ///
    /// #1091. Scoped to STANDARD (non-extended, 32-byte) Ed25519 keys —
    /// verified live against real cardano-cli 11.0.0.0 on both a payment
    /// key (`PaymentSigningKeyShelley_ed25519` ->
    /// `PaymentVerificationKeyShelley_ed25519`) and a DRep key
    /// (`DRepSigningKey_ed25519` -> `DRepVerificationKey_ed25519`), same
    /// output bytes both times, output `type` = input `type` with
    /// `"SigningKey"` replaced by `"VerificationKey"`. An EXTENDED
    /// (BIP32-derived) signing key needs Ed25519-BIP32 scalar
    /// multiplication to derive its verification key, which this repo has
    /// no implementation of yet (the same gap `key derive-from-mnemonic`
    /// is deferred for) — rejected with a clear error rather than silently
    /// producing a wrong key.
    VerificationKey {
        #[arg(long = "signing-key-file", value_name = "FILEPATH")]
        signing_key_file: PathBuf,
        #[arg(long = "verification-key-file", value_name = "FILEPATH")]
        verification_key_file: PathBuf,
    },
    /// Get a non-extended verification key from an extended verification
    /// key. This supports all extended key types.
    ///
    /// #1091. The truncation itself (drop the trailing 32-byte chain code,
    /// keep the leading 32-byte pubkey) is the SAME operation
    /// `credential::vkey_bytes_to_hash` already performs when hashing an
    /// extended key elsewhere in this crate. The output `type` name is
    /// derived by removing `"Extended"` from the input type — inferred
    /// from cardano-cli's own naming convention (e.g. the
    /// `PaymentExtendedVerificationKeyShelley_ed25519_bip32` family this
    /// session's `key convert-byron-key` work established), NOT
    /// independently re-verified against a live capture: producing a real
    /// extended verification key needs BIP-32 derivation tooling this
    /// session did not implement, so there was no live fixture to test
    /// against.
    NonExtendedKey {
        #[arg(long = "extended-verification-key-file", value_name = "FILEPATH")]
        extended_verification_key_file: PathBuf,
        #[arg(long = "verification-key-file", value_name = "FILEPATH")]
        verification_key_file: PathBuf,
    },
}

/// The `type` string cardano-cli writes for a re-wrapped Byron key, chosen
/// purely from the selected `--byron-*-key-type` flag — see
/// `KeySubcommand::ConvertByronKey`'s doc comment for how this table was
/// captured (real binary output, not documentation).
fn convert_byron_key_output_type(
    is_genesis: bool,
    is_genesis_delegate: bool,
    is_verification: bool,
) -> &'static str {
    match (is_genesis, is_genesis_delegate, is_verification) {
        (false, false, false) => "PaymentSigningKeyByron_ed25519_bip32",
        (false, false, true) => "PaymentVerificationKeyByron_ed25519_bip32",
        (true, false, false) => "GenesisExtendedSigningKey_ed25519_bip32",
        (true, false, true) => "GenesisExtendedVerificationKey_ed25519_bip32",
        (false, true, false) => "GenesisDelegateExtendedSigningKey_ed25519_bip32",
        (false, true, true) => "GenesisDelegateExtendedVerificationKey_ed25519_bip32",
        (true, true, _) => unreachable!("clap enforces the three key-type flags are exclusive"),
    }
}

/// Load the raw byte payload of a Byron signing- or verification-key file
/// for `key convert-byron-key`. Two forms are accepted, matching what a
/// real cardano-cli 11.0.1 `--byron-verification-key-file` actually
/// produces/consumes (verified: `byron key to-verification --to FILE`
/// writes a bare Base64 string, no JSON) alongside dugite-cli's own
/// text-envelope convention (`type`/`cborHex`, the shape
/// `credential::load_vkey_bytes_from_envelope` already reads for every
/// other command in this crate):
///   1. JSON text envelope — `cborHex`'s CBOR byte-string payload, AS-IS
///      (no length normalization: cardano-cli passes 96- and 128-byte
///      payloads through unchanged, confirmed empirically).
///   2. A bare Base64 string as the file's entire trimmed content.
///
/// Real cardano-cli's Byron `--secret`/native on-disk format (raw CBOR
/// binary, no JSON, no Base64 — what `cardano-cli byron key keygen`
/// itself writes) is NOT accepted; `byron key keygen` is a WONTFIX ceremony
/// command dugite-cli does not implement, so no dugite tooling ever
/// produces that shape either.
fn load_byron_key_bytes(path: &Path) -> Result<Vec<u8>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", path.display()))?;
    let trimmed = content.trim();
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return load_vkey_bytes_from_envelope(path);
    }
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(trimmed)
        .map_err(|e| {
            anyhow::anyhow!(
                "'{}' is neither a JSON text envelope nor a valid Base64 Byron key: {e}",
                path.display()
            )
        })
}

impl KeyCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            KeySubcommand::GeneratePaymentKey {
                signing_key_file,
                verification_key_file,
            } => {
                let sk = PaymentSigningKey::generate();
                let vk = sk.verification_key();

                let sk_envelope = TextEnvelope::payment_signing_key(&sk);
                let vk_envelope = TextEnvelope::payment_verification_key(&vk);

                let sk_json = serde_json::to_string_pretty(&sk_envelope)?;
                let vk_json = serde_json::to_string_pretty(&vk_envelope)?;

                std::fs::write(&signing_key_file, sk_json)?;
                std::fs::write(&verification_key_file, vk_json)?;

                println!(
                    "Payment signing key written to: {}",
                    signing_key_file.display()
                );
                println!(
                    "Payment verification key written to: {}",
                    verification_key_file.display()
                );
                Ok(())
            }
            KeySubcommand::GenerateStakeKey {
                signing_key_file,
                verification_key_file,
            } => {
                let sk = PaymentSigningKey::generate();
                let vk = sk.verification_key();

                let sk_envelope = TextEnvelope::stake_signing_key(&sk);
                let vk_envelope = TextEnvelope::stake_verification_key(&vk);

                let sk_json = serde_json::to_string_pretty(&sk_envelope)?;
                let vk_json = serde_json::to_string_pretty(&vk_envelope)?;

                std::fs::write(&signing_key_file, sk_json)?;
                std::fs::write(&verification_key_file, vk_json)?;

                println!(
                    "Stake signing key written to: {}",
                    signing_key_file.display()
                );
                println!(
                    "Stake verification key written to: {}",
                    verification_key_file.display()
                );
                Ok(())
            }
            KeySubcommand::VerificationKeyHash {
                verification_key_file,
            } => {
                let content = std::fs::read_to_string(&verification_key_file)?;
                let envelope: TextEnvelope = serde_json::from_str(&content)?;

                // cardano-cli validates the envelope type; hashing whatever
                // 32-byte payload arrives would silently hash SIGNING keys
                // (printing a value nothing on chain matches) and KES/VRF
                // keys (which use different hash conventions) (#934).
                if !ACCEPTED_VKEY_TYPES.contains(&envelope.type_.as_str()) {
                    anyhow::bail!(
                        "envelope type \"{}\" is not an accepted verification key type; \
                         accepted types: {}",
                        envelope.type_,
                        ACCEPTED_VKEY_TYPES.join(", ")
                    );
                }

                let cbor_bytes = hex::decode(&envelope.cbor_hex)?;
                // Extract the raw key bytes from CBOR wrapper
                let key_bytes = if cbor_bytes.len() > 2 {
                    &cbor_bytes[2..] // Skip CBOR byte string header
                } else {
                    &cbor_bytes
                };

                let vk = dugite_crypto::keys::PaymentVerificationKey::from_bytes(key_bytes)?;
                let hash = vk.hash();

                println!("{}", hash.to_hex());
                Ok(())
            }
            KeySubcommand::ConvertByronKey {
                byron_payment_key_type,
                legacy_byron_payment_key_type,
                byron_genesis_key_type,
                legacy_byron_genesis_key_type,
                byron_genesis_delegate_key_type,
                legacy_byron_genesis_delegate_key_type,
                byron_signing_key_file,
                byron_verification_key_file,
                password,
                out_file,
            } => {
                let flags = [
                    byron_payment_key_type,
                    legacy_byron_payment_key_type,
                    byron_genesis_key_type,
                    legacy_byron_genesis_key_type,
                    byron_genesis_delegate_key_type,
                    legacy_byron_genesis_delegate_key_type,
                ];
                if flags.iter().filter(|f| **f).count() != 1 {
                    bail!(
                        "pass exactly one of --byron-payment-key-type, \
                         --legacy-byron-payment-key-type, --byron-genesis-key-type, \
                         --legacy-byron-genesis-key-type, --byron-genesis-delegate-key-type, or \
                         --legacy-byron-genesis-delegate-key-type"
                    );
                }
                if password.is_some() {
                    bail!(
                        "--password: encrypted Byron keys are not supported (no cardano-sl \
                         password-based key decryption implemented)"
                    );
                }
                if legacy_byron_payment_key_type
                    || legacy_byron_genesis_key_type
                    || legacy_byron_genesis_delegate_key_type
                {
                    bail!(
                        "--legacy-byron-*-key-type: the legacy cardano-sl on-disk key format is \
                         not supported (verified against real cardano-cli: it is a different, \
                         older binary encoding than the current Byron key format, and this repo \
                         has no parser for it)"
                    );
                }
                let is_genesis = byron_genesis_key_type;
                let is_genesis_delegate = byron_genesis_delegate_key_type;

                let (bytes, is_verification) = if let Some(path) = byron_signing_key_file {
                    (load_byron_key_bytes(&path)?, false)
                } else if let Some(path) = byron_verification_key_file {
                    (load_byron_key_bytes(&path)?, true)
                } else {
                    bail!(
                        "missing selector: pass one of --byron-signing-key-file or \
                         --byron-verification-key-file"
                    );
                };

                let type_str =
                    convert_byron_key_output_type(is_genesis, is_genesis_delegate, is_verification);
                let envelope = serde_json::json!({
                    "type": type_str,
                    "description": "",
                    "cborHex": hex::encode(cbor_wrap_bytes(&bytes)),
                });
                std::fs::write(&out_file, serde_json::to_string_pretty(&envelope)?).map_err(
                    |e| anyhow::anyhow!("failed to write '{}': {e}", out_file.display()),
                )?;
                Ok(())
            }
            KeySubcommand::ConvertByronGenesisVkey {
                byron_genesis_verification_key,
                out_file,
            } => {
                use base64::Engine;
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(byron_genesis_verification_key.trim())
                    .map_err(|e| anyhow::anyhow!("invalid Base64 verification key: {e}"))?;
                if raw.len() < 32 {
                    bail!(
                        "expected at least 32 bytes in the Byron genesis verification key, got {}",
                        raw.len()
                    );
                }
                let pubkey = &raw[..32];
                let envelope = serde_json::json!({
                    "type": "GenesisVerificationKey_ed25519",
                    "description": "",
                    "cborHex": hex::encode(cbor_wrap_bytes(pubkey)),
                });
                std::fs::write(&out_file, serde_json::to_string_pretty(&envelope)?).map_err(
                    |e| anyhow::anyhow!("failed to write '{}': {e}", out_file.display()),
                )?;
                Ok(())
            }
            KeySubcommand::VerificationKey {
                signing_key_file,
                verification_key_file,
            } => {
                let content = std::fs::read_to_string(&signing_key_file).map_err(|e| {
                    anyhow::anyhow!("failed to read '{}': {e}", signing_key_file.display())
                })?;
                let envelope: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                    anyhow::anyhow!("'{}' is not valid JSON: {e}", signing_key_file.display())
                })?;
                let type_str = envelope
                    .get("type")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("missing 'type' in signing key envelope"))?;
                if !type_str.contains("SigningKey") {
                    bail!(
                        "'{}' does not look like a signing-key envelope (type '{type_str}')",
                        signing_key_file.display()
                    );
                }
                let sk_bytes = load_vkey_bytes_from_envelope(&signing_key_file)?;
                if sk_bytes.len() != 32 {
                    bail!(
                        "extended (BIP32-derived, {}-byte) signing keys are not supported by \
                         dugite-cli's `key verification-key` yet — only standard 32-byte \
                         Ed25519 keys",
                        sk_bytes.len()
                    );
                }
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&sk_bytes);
                let sk = PaymentSigningKey::from_bytes(&arr)?;
                let vk_bytes = sk.verification_key().to_bytes();

                let out_type = type_str.replacen("SigningKey", "VerificationKey", 1);
                let envelope = serde_json::json!({
                    "type": out_type,
                    "description": "",
                    "cborHex": hex::encode(cbor_wrap_bytes(&vk_bytes)),
                });
                std::fs::write(
                    &verification_key_file,
                    serde_json::to_string_pretty(&envelope)?,
                )
                .map_err(|e| {
                    anyhow::anyhow!("failed to write '{}': {e}", verification_key_file.display())
                })?;
                Ok(())
            }
            KeySubcommand::NonExtendedKey {
                extended_verification_key_file,
                verification_key_file,
            } => {
                let content =
                    std::fs::read_to_string(&extended_verification_key_file).map_err(|e| {
                        anyhow::anyhow!(
                            "failed to read '{}': {e}",
                            extended_verification_key_file.display()
                        )
                    })?;
                let envelope: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                    anyhow::anyhow!(
                        "'{}' is not valid JSON: {e}",
                        extended_verification_key_file.display()
                    )
                })?;
                let type_str = envelope
                    .get("type")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        anyhow::anyhow!("missing 'type' in verification key envelope")
                    })?;
                let bytes = load_vkey_bytes_from_envelope(&extended_verification_key_file)?;
                if bytes.len() != 64 {
                    bail!(
                        "expected a 64-byte extended verification key (pubkey || chain code), \
                         got {} bytes",
                        bytes.len()
                    );
                }
                let pubkey = &bytes[..32];
                let out_type = type_str.replacen("Extended", "", 1);
                let envelope = serde_json::json!({
                    "type": out_type,
                    "description": "",
                    "cborHex": hex::encode(cbor_wrap_bytes(pubkey)),
                });
                std::fs::write(
                    &verification_key_file,
                    serde_json::to_string_pretty(&envelope)?,
                )
                .map_err(|e| {
                    anyhow::anyhow!("failed to write '{}': {e}", verification_key_file.display())
                })?;
                Ok(())
            }
        }
    }
}

/// Wrap bytes in a CBOR byte string (major type 2) header — mirrors
/// `byron.rs`'s private `cbor_wrap`, duplicated locally rather than made
/// `pub(crate)` there to avoid widening that module's surface for a single
/// caller outside it.
fn cbor_wrap_bytes(data: &[u8]) -> Vec<u8> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use dugite_crypto::keys::PaymentVerificationKey;

    /// Parse a text-envelope JSON file and return (envelope, raw key bytes).
    /// The cborHex is `0x5820 ++ raw32` for 32-byte Ed25519 keys.
    fn read_key_envelope(path: &std::path::Path) -> (TextEnvelope, Vec<u8>) {
        let content = std::fs::read_to_string(path).unwrap();
        let envelope: TextEnvelope = serde_json::from_str(&content).unwrap();
        let cbor = hex::decode(&envelope.cbor_hex).unwrap();
        assert_eq!(&cbor[..2], &[0x58, 0x20], "expected CBOR bytes(32) header");
        (envelope, cbor[2..].to_vec())
    }

    #[test]
    fn generate_payment_key_writes_matching_envelope_pair() {
        let dir = tempfile::tempdir().unwrap();
        let sk_path = dir.path().join("payment.skey");
        let vk_path = dir.path().join("payment.vkey");

        KeyCmd {
            command: KeySubcommand::GeneratePaymentKey {
                signing_key_file: sk_path.clone(),
                verification_key_file: vk_path.clone(),
            },
        }
        .run()
        .unwrap();

        let (sk_env, sk_bytes) = read_key_envelope(&sk_path);
        let (vk_env, vk_bytes) = read_key_envelope(&vk_path);
        assert_eq!(sk_env.type_, "PaymentSigningKeyShelley_ed25519");
        assert_eq!(vk_env.type_, "PaymentVerificationKeyShelley_ed25519");

        // The written verification key must actually derive from the written
        // signing key — otherwise the pair is unusable for signing.
        let sk = PaymentSigningKey::from_bytes(&sk_bytes).unwrap();
        assert_eq!(sk.verification_key().to_bytes().to_vec(), vk_bytes);
    }

    #[test]
    fn generate_stake_key_uses_stake_envelope_types() {
        let dir = tempfile::tempdir().unwrap();
        let sk_path = dir.path().join("stake.skey");
        let vk_path = dir.path().join("stake.vkey");

        KeyCmd {
            command: KeySubcommand::GenerateStakeKey {
                signing_key_file: sk_path.clone(),
                verification_key_file: vk_path.clone(),
            },
        }
        .run()
        .unwrap();

        let (sk_env, sk_bytes) = read_key_envelope(&sk_path);
        let (vk_env, vk_bytes) = read_key_envelope(&vk_path);
        assert_eq!(sk_env.type_, "StakeSigningKeyShelley_ed25519");
        assert_eq!(vk_env.type_, "StakeVerificationKeyShelley_ed25519");

        let sk = PaymentSigningKey::from_bytes(&sk_bytes).unwrap();
        assert_eq!(sk.verification_key().to_bytes().to_vec(), vk_bytes);
    }

    #[test]
    fn verification_key_hash_accepts_generated_vkey() {
        let dir = tempfile::tempdir().unwrap();
        let sk_path = dir.path().join("payment.skey");
        let vk_path = dir.path().join("payment.vkey");

        KeyCmd {
            command: KeySubcommand::GeneratePaymentKey {
                signing_key_file: sk_path,
                verification_key_file: vk_path.clone(),
            },
        }
        .run()
        .unwrap();

        // Sanity-check the hash the command would print: re-derive it from the
        // file contents through the same envelope format.
        let (_, vk_bytes) = read_key_envelope(&vk_path);
        let vk = PaymentVerificationKey::from_bytes(&vk_bytes).unwrap();
        assert_eq!(vk.hash().as_bytes().len(), 28);

        KeyCmd {
            command: KeySubcommand::VerificationKeyHash {
                verification_key_file: vk_path,
            },
        }
        .run()
        .unwrap();
    }

    #[test]
    fn verification_key_hash_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = KeyCmd {
            command: KeySubcommand::VerificationKeyHash {
                verification_key_file: dir.path().join("does-not-exist.vkey"),
            },
        }
        .run();
        assert!(result.is_err(), "missing vkey file must be an error");
    }

    #[test]
    fn verification_key_hash_malformed_json_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.vkey");
        std::fs::write(&path, "this is not json {").unwrap();
        let result = KeyCmd {
            command: KeySubcommand::VerificationKeyHash {
                verification_key_file: path,
            },
        }
        .run();
        assert!(result.is_err(), "malformed JSON must be an error");
    }

    #[test]
    fn verification_key_hash_bad_hex_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("badhex.vkey");
        let env = serde_json::json!({
            "type": "PaymentVerificationKeyShelley_ed25519",
            "description": "",
            "cborHex": "not-hex-at-all"
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        let result = KeyCmd {
            command: KeySubcommand::VerificationKeyHash {
                verification_key_file: path,
            },
        }
        .run();
        assert!(result.is_err(), "non-hex cborHex must be an error");
    }

    /// Write an envelope with an arbitrary `type` and 32-byte `cborHex`
    /// payload, returning the path.
    fn write_envelope(dir: &std::path::Path, name: &str, type_: &str, raw32: [u8; 32]) -> PathBuf {
        let path = dir.join(name);
        let env = serde_json::json!({
            "type": type_,
            "description": "",
            "cborHex": format!("5820{}", hex::encode(raw32)),
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        path
    }

    /// Write an envelope with an arbitrary `type` and a valid bytes(32)
    /// cborHex, returning the path.
    fn write_typed_envelope(dir: &std::path::Path, name: &str, type_: &str) -> PathBuf {
        let path = dir.join(name);
        let env = serde_json::json!({
            "type": type_,
            "description": "",
            "cborHex": format!("5820{}", hex::encode([0x2Au8; 32]))
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        path
    }

    /// A signing-key envelope must be rejected even though its payload is a
    /// hashable 32-byte string — NEVER hash a signing key silently. The
    /// error must name the offending type.
    #[test]
    fn verification_key_hash_rejects_signing_key_types() {
        let dir = tempfile::tempdir().unwrap();
        for type_ in [
            "PaymentSigningKeyShelley_ed25519",
            "StakeSigningKeyShelley_ed25519",
            "StakePoolSigningKey_ed25519",
            "GenesisSigningKey_ed25519",
            "DRepSigningKey_ed25519",
        ] {
            let path = write_typed_envelope(dir.path(), &format!("{type_}.skey"), type_);
            let err = KeyCmd {
                command: KeySubcommand::VerificationKeyHash {
                    verification_key_file: path,
                },
            }
            .run()
            .expect_err(&format!("{type_} must be rejected"));
            assert!(
                err.to_string().contains(type_),
                "error must name the offending type {type_}, got: {err}"
            );
        }
    }

    /// KES/VRF verification keys use different hash conventions (VRF key
    /// hashes are blake2b-256 via `node key-hash-VRF`); accepting them here
    /// would silently print a wrong-width hash.
    #[test]
    fn verification_key_hash_rejects_kes_and_vrf_types() {
        let dir = tempfile::tempdir().unwrap();
        for type_ in [
            "KesVerificationKey_ed25519_kes_2^6",
            "VrfVerificationKey_PraosVRF",
        ] {
            let path = write_typed_envelope(dir.path(), &format!("{type_}.vkey"), type_);
            let err = KeyCmd {
                command: KeySubcommand::VerificationKeyHash {
                    verification_key_file: path,
                },
            }
            .run()
            .expect_err(&format!("{type_} must be rejected"));
            assert!(
                err.to_string().contains(type_),
                "error must name the offending type {type_}, got: {err}"
            );
        }
    }

    /// Every Ed25519 verification-key envelope type dugite-cli writes (plus
    /// the cardano-cli conventional ones) must be accepted.
    #[test]
    fn verification_key_hash_accepts_all_ed25519_vkey_types() {
        let dir = tempfile::tempdir().unwrap();
        for (i, type_) in ACCEPTED_VKEY_TYPES.iter().enumerate() {
            let path = write_typed_envelope(dir.path(), &format!("k{i}.vkey"), type_);
            KeyCmd {
                command: KeySubcommand::VerificationKeyHash {
                    verification_key_file: path,
                },
            }
            .run()
            .unwrap_or_else(|e| panic!("{type_} must be accepted, got: {e}"));
        }
    }

    #[test]
    fn verification_key_hash_wrong_key_length_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("short.vkey");
        // CBOR bytes(16) — a 16-byte payload is not a valid Ed25519 key.
        let env = serde_json::json!({
            "type": "PaymentVerificationKeyShelley_ed25519",
            "description": "",
            "cborHex": format!("5810{}", hex::encode([0u8; 16]))
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        let result = KeyCmd {
            command: KeySubcommand::VerificationKeyHash {
                verification_key_file: path,
            },
        }
        .run();
        assert!(result.is_err(), "16-byte key must be rejected");
    }

    // ── `key convert-byron-genesis-vkey` — #1091 ─────────────────────────

    /// Byte-exact against a real cardano-cli 11.0.0.0
    /// `key convert-byron-genesis-vkey` capture (2026-08-21): a 64-byte
    /// Byron extended vkey Base64-encoded inline, only the leading 32 bytes
    /// survive, wrapped as `GenesisVerificationKey_ed25519`.
    #[test]
    fn convert_byron_genesis_vkey_matches_real_cardano_cli_capture() {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.vkey");
        KeyCmd {
            command: KeySubcommand::ConvertByronGenesisVkey {
                byron_genesis_verification_key:
                    "kaynTZpiJ9Z0/ByIoYoOCPGIQaRRqnPbQ5PKZoPr4WtkF1hFuwjY+0p4JXxz+CMrWFdyLKGEtXZPaWoIBHeudA=="
                        .to_string(),
                out_file: out_path.clone(),
            },
        }
        .run()
        .unwrap();
        let content = std::fs::read_to_string(&out_path).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(env["type"], "GenesisVerificationKey_ed25519");
        assert_eq!(
            env["cborHex"],
            "582091aca74d9a6227d674fc1c88a18a0e08f18841a451aa73db4393ca6683ebe16b"
        );
    }

    #[test]
    fn convert_byron_genesis_vkey_rejects_short_input() {
        use base64::Engine;
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.vkey");
        let result = KeyCmd {
            command: KeySubcommand::ConvertByronGenesisVkey {
                byron_genesis_verification_key: base64::engine::general_purpose::STANDARD
                    .encode([0u8; 10]),
                out_file: out_path,
            },
        }
        .run();
        assert!(result.is_err(), "10-byte input must be rejected");
    }

    #[test]
    fn convert_byron_genesis_vkey_rejects_bad_base64() {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.vkey");
        let result = KeyCmd {
            command: KeySubcommand::ConvertByronGenesisVkey {
                byron_genesis_verification_key: "not valid base64!!!".to_string(),
                out_file: out_path,
            },
        }
        .run();
        assert!(result.is_err());
    }

    // ── `key convert-byron-key` — #1091 ──────────────────────────────────

    fn byron_key_cmd(
        byron_payment_key_type: bool,
        byron_genesis_key_type: bool,
        byron_genesis_delegate_key_type: bool,
        byron_signing_key_file: Option<PathBuf>,
        byron_verification_key_file: Option<PathBuf>,
        password: Option<String>,
        out_file: PathBuf,
    ) -> KeySubcommand {
        KeySubcommand::ConvertByronKey {
            byron_payment_key_type,
            legacy_byron_payment_key_type: false,
            byron_genesis_key_type,
            legacy_byron_genesis_key_type: false,
            byron_genesis_delegate_key_type,
            legacy_byron_genesis_delegate_key_type: false,
            byron_signing_key_file,
            byron_verification_key_file,
            password,
            out_file,
        }
    }

    /// The synthetic 128-byte Byron signing-key envelope used across these
    /// tests — the SAME bytes captured from a real
    /// `cardano-cli byron key keygen` output, re-wrapped as dugite-cli's own
    /// JSON text-envelope convention (see `load_byron_key_bytes`'s doc for
    /// why the raw cardano-sl on-disk format itself is out of scope).
    fn write_synthetic_byron_skey(dir: &std::path::Path) -> PathBuf {
        let path = dir.join("byron.skey");
        let env = serde_json::json!({
            "type": "PaymentSigningKeyByron_ed25519_bip32",
            "description": "",
            "cborHex": "588008a6b188b687fb83b65e3f0e7a53619038bd2704fcaefb6616938fe07e85cd590b208b5e9ca9a479b921500b4d4c460c77c052d4be9a1d1fd4ac5471414d96f791aca74d9a6227d674fc1c88a18a0e08f18841a451aa73db4393ca6683ebe16b64175845bb08d8fb4a78257c73f8232b5857722ca184b5764f696a080477ae74"
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        path
    }

    /// Byte-exact against a real cardano-cli 11.0.0.0
    /// `key convert-byron-key --byron-payment-key-type
    /// --byron-signing-key-file` capture (2026-08-21): bytes pass through
    /// UNCHANGED (128 bytes in, 128 bytes out), only the envelope `type`
    /// changes.
    #[test]
    fn convert_byron_key_payment_signing_matches_real_cardano_cli_capture() {
        let dir = tempfile::tempdir().unwrap();
        let skey = write_synthetic_byron_skey(dir.path());
        let out_path = dir.path().join("out.skey");
        KeyCmd {
            command: byron_key_cmd(true, false, false, Some(skey), None, None, out_path.clone()),
        }
        .run()
        .unwrap();
        let content = std::fs::read_to_string(&out_path).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(env["type"], "PaymentSigningKeyByron_ed25519_bip32");
        assert_eq!(
            env["cborHex"],
            "588008a6b188b687fb83b65e3f0e7a53619038bd2704fcaefb6616938fe07e85cd590b208b5e9ca9a479b921500b4d4c460c77c052d4be9a1d1fd4ac5471414d96f791aca74d9a6227d674fc1c88a18a0e08f18841a451aa73db4393ca6683ebe16b64175845bb08d8fb4a78257c73f8232b5857722ca184b5764f696a080477ae74"
        );
    }

    /// Byte-exact against a real cardano-cli capture of
    /// `--byron-genesis-delegate-key-type --byron-signing-key-file`
    /// (2026-08-21) — confirms the OUTPUT TYPE LABEL alone changes across
    /// the three key-type families; the input bytes are identical to the
    /// payment-type test above.
    #[test]
    fn convert_byron_key_genesis_delegate_signing_matches_real_cardano_cli_capture() {
        let dir = tempfile::tempdir().unwrap();
        let skey = write_synthetic_byron_skey(dir.path());
        let out_path = dir.path().join("out.skey");
        KeyCmd {
            command: byron_key_cmd(false, false, true, Some(skey), None, None, out_path.clone()),
        }
        .run()
        .unwrap();
        let content = std::fs::read_to_string(&out_path).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            env["type"],
            "GenesisDelegateExtendedSigningKey_ed25519_bip32"
        );
    }

    /// Byte-exact against a real cardano-cli capture of
    /// `--byron-payment-key-type --byron-verification-key-file` on a bare
    /// Base64 vkey file (2026-08-21) — exercises the non-JSON input path.
    #[test]
    fn convert_byron_key_verification_from_bare_base64_file_matches_real_cardano_cli_capture() {
        let dir = tempfile::tempdir().unwrap();
        let vkey_path = dir.path().join("byron.vkey");
        std::fs::write(
            &vkey_path,
            "kaynTZpiJ9Z0/ByIoYoOCPGIQaRRqnPbQ5PKZoPr4WtkF1hFuwjY+0p4JXxz+CMrWFdyLKGEtXZPaWoIBHeudA==",
        )
        .unwrap();
        let out_path = dir.path().join("out.vkey");
        KeyCmd {
            command: byron_key_cmd(
                true,
                false,
                false,
                None,
                Some(vkey_path),
                None,
                out_path.clone(),
            ),
        }
        .run()
        .unwrap();
        let content = std::fs::read_to_string(&out_path).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(env["type"], "PaymentVerificationKeyByron_ed25519_bip32");
        assert_eq!(
            env["cborHex"],
            "584091aca74d9a6227d674fc1c88a18a0e08f18841a451aa73db4393ca6683ebe16b64175845bb08d8fb4a78257c73f8232b5857722ca184b5764f696a080477ae74"
        );
    }

    #[test]
    fn convert_byron_key_requires_exactly_one_type_flag() {
        let dir = tempfile::tempdir().unwrap();
        let skey = write_synthetic_byron_skey(dir.path());
        let out_path = dir.path().join("out.skey");
        // Zero flags set.
        let result = KeyCmd {
            command: byron_key_cmd(
                false,
                false,
                false,
                Some(skey.clone()),
                None,
                None,
                out_path.clone(),
            ),
        }
        .run();
        assert!(result.is_err(), "zero type flags must be rejected");

        // Two flags set.
        let result = KeyCmd {
            command: byron_key_cmd(true, true, false, Some(skey), None, None, out_path),
        }
        .run();
        assert!(result.is_err(), "two type flags must be rejected");
    }

    #[test]
    fn convert_byron_key_rejects_password() {
        let dir = tempfile::tempdir().unwrap();
        let skey = write_synthetic_byron_skey(dir.path());
        let out_path = dir.path().join("out.skey");
        let result = KeyCmd {
            command: byron_key_cmd(
                true,
                false,
                false,
                Some(skey),
                None,
                Some("hunter2".to_string()),
                out_path,
            ),
        }
        .run();
        let err = result.expect_err("--password must be rejected");
        assert!(err.to_string().contains("password"));
    }

    #[test]
    fn convert_byron_key_rejects_legacy_type() {
        let dir = tempfile::tempdir().unwrap();
        let skey = write_synthetic_byron_skey(dir.path());
        let out_path = dir.path().join("out.skey");
        let cmd = KeySubcommand::ConvertByronKey {
            byron_payment_key_type: false,
            legacy_byron_payment_key_type: true,
            byron_genesis_key_type: false,
            legacy_byron_genesis_key_type: false,
            byron_genesis_delegate_key_type: false,
            legacy_byron_genesis_delegate_key_type: false,
            byron_signing_key_file: Some(skey),
            byron_verification_key_file: None,
            password: None,
            out_file: out_path,
        };
        let err = KeyCmd { command: cmd }
            .run()
            .expect_err("legacy type must be rejected");
        assert!(err.to_string().contains("legacy"));
    }

    #[test]
    fn convert_byron_key_requires_a_file_selector() {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.skey");
        let result = KeyCmd {
            command: byron_key_cmd(true, false, false, None, None, None, out_path),
        }
        .run();
        let err = result.expect_err("missing file selector must be rejected");
        assert!(err.to_string().contains("missing selector"));
    }

    // ── `key verification-key` / `key non-extended-key` — #1091 ─────────

    /// Byte-exact against a real cardano-cli 11.0.0.0
    /// `key verification-key` capture (2026-08-21) on a payment key.
    #[test]
    fn verification_key_payment_matches_real_cardano_cli_capture() {
        let dir = tempfile::tempdir().unwrap();
        let sk_path = write_envelope(
            dir.path(),
            "pay.skey",
            "PaymentSigningKeyShelley_ed25519",
            [
                0xd4, 0x25, 0xd2, 0x3a, 0x30, 0x08, 0x63, 0x8e, 0x2b, 0x0e, 0x6a, 0xc4, 0xdc, 0x4d,
                0xe6, 0xd0, 0xc0, 0x1c, 0xea, 0x39, 0xe6, 0x5e, 0x8f, 0xdb, 0x11, 0x36, 0xf3, 0x8e,
                0x18, 0xf1, 0xf8, 0x1a,
            ],
        );
        let vk_path = dir.path().join("pay.vkey");
        KeyCmd {
            command: KeySubcommand::VerificationKey {
                signing_key_file: sk_path,
                verification_key_file: vk_path.clone(),
            },
        }
        .run()
        .unwrap();
        let content = std::fs::read_to_string(&vk_path).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(env["type"], "PaymentVerificationKeyShelley_ed25519");
    }

    #[test]
    fn verification_key_type_name_derived_by_substitution() {
        let dir = tempfile::tempdir().unwrap();
        let sk_path = write_envelope(
            dir.path(),
            "drep.skey",
            "DRepSigningKey_ed25519",
            [0x11; 32],
        );
        let vk_path = dir.path().join("drep.vkey");
        KeyCmd {
            command: KeySubcommand::VerificationKey {
                signing_key_file: sk_path,
                verification_key_file: vk_path.clone(),
            },
        }
        .run()
        .unwrap();
        let content = std::fs::read_to_string(&vk_path).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(env["type"], "DRepVerificationKey_ed25519");
    }

    #[test]
    fn verification_key_rejects_extended_signing_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ext.skey");
        let env = serde_json::json!({
            "type": "PaymentExtendedSigningKeyShelley_ed25519_bip32",
            "description": "",
            "cborHex": format!("5860{}", hex::encode([0x22u8; 96])),
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        let vk_path = dir.path().join("out.vkey");
        let err = KeyCmd {
            command: KeySubcommand::VerificationKey {
                signing_key_file: path,
                verification_key_file: vk_path,
            },
        }
        .run()
        .unwrap_err();
        assert!(err.to_string().contains("extended"));
    }

    #[test]
    fn verification_key_rejects_non_signing_key_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_envelope(
            dir.path(),
            "already.vkey",
            "PaymentVerificationKeyShelley_ed25519",
            [0x33; 32],
        );
        let vk_path = dir.path().join("out.vkey");
        let err = KeyCmd {
            command: KeySubcommand::VerificationKey {
                signing_key_file: path,
                verification_key_file: vk_path,
            },
        }
        .run()
        .unwrap_err();
        assert!(err.to_string().contains("does not look like"));
    }

    #[test]
    fn non_extended_key_truncates_to_pubkey_and_strips_extended_from_type() {
        let dir = tempfile::tempdir().unwrap();
        let mut raw64 = vec![0xabu8; 32];
        raw64.extend_from_slice(&[0xcdu8; 32]);
        let path = dir.path().join("ext.vkey");
        let env = serde_json::json!({
            "type": "PaymentExtendedVerificationKeyShelley_ed25519_bip32",
            "description": "",
            "cborHex": format!("5840{}", hex::encode(&raw64)),
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        let out_path = dir.path().join("out.vkey");
        KeyCmd {
            command: KeySubcommand::NonExtendedKey {
                extended_verification_key_file: path,
                verification_key_file: out_path.clone(),
            },
        }
        .run()
        .unwrap();
        let content = std::fs::read_to_string(&out_path).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(
            env["type"], "PaymentVerificationKeyShelley_ed25519_bip32",
            "'Extended' must be stripped from the type name"
        );
        assert_eq!(
            env["cborHex"],
            format!("5820{}", hex::encode([0xabu8; 32])),
            "only the leading 32 bytes (pubkey) survive, chain code dropped"
        );
    }

    #[test]
    fn non_extended_key_rejects_wrong_length() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_envelope(
            dir.path(),
            "short.vkey",
            "PaymentExtendedVerificationKeyShelley_ed25519_bip32",
            [0x44; 32],
        );
        let out_path = dir.path().join("out.vkey");
        let err = KeyCmd {
            command: KeySubcommand::NonExtendedKey {
                extended_verification_key_file: path,
                verification_key_file: out_path,
            },
        }
        .run()
        .unwrap_err();
        assert!(err.to_string().contains("64-byte"));
    }
}
