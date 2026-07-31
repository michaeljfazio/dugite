use anyhow::Result;
use clap::{Args, Subcommand};
use dugite_crypto::keys::{PaymentSigningKey, TextEnvelope};
use std::path::PathBuf;

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
        }
    }
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
}
