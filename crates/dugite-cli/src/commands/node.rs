use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct NodeCmd {
    #[command(subcommand)]
    command: NodeSubcommand,
}

#[derive(Subcommand, Debug)]
enum NodeSubcommand {
    /// Generate node cold keys
    KeyGen {
        #[arg(long)]
        cold_verification_key_file: PathBuf,
        #[arg(long)]
        cold_signing_key_file: PathBuf,
        #[arg(long)]
        operational_certificate_counter_file: PathBuf,
    },
    /// Generate a KES key pair
    KeyGenKes {
        #[arg(long)]
        verification_key_file: PathBuf,
        #[arg(long)]
        signing_key_file: PathBuf,
    },
    /// Generate a VRF key pair
    KeyGenVrf {
        #[arg(long)]
        verification_key_file: PathBuf,
        #[arg(long)]
        signing_key_file: PathBuf,
    },
    /// Issue a new operational certificate
    IssueOpCert {
        #[arg(long)]
        kes_verification_key_file: PathBuf,
        #[arg(long)]
        cold_signing_key_file: PathBuf,
        #[arg(long)]
        operational_certificate_counter_file: PathBuf,
        #[arg(long)]
        kes_period: u64,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a new operational certificate issue counter
    NewCounter {
        #[arg(long)]
        cold_verification_key_file: PathBuf,
        #[arg(long)]
        counter_value: u64,
        #[arg(long)]
        operational_certificate_counter_file: PathBuf,
    },
    /// Get the hash of a VRF verification key
    KeyHashVrf {
        #[arg(long)]
        verification_key_file: PathBuf,
    },
}

fn simple_cbor_wrap(data: &[u8]) -> Vec<u8> {
    let mut result = Vec::new();
    if data.len() < 24 {
        result.push(0x40 | data.len() as u8);
    } else if data.len() < 256 {
        result.push(0x58);
        result.push(data.len() as u8);
    } else {
        result.push(0x59);
        result.extend_from_slice(&(data.len() as u16).to_be_bytes());
    }
    result.extend_from_slice(data);
    result
}

impl NodeCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            NodeSubcommand::KeyGen {
                cold_verification_key_file,
                cold_signing_key_file,
                operational_certificate_counter_file,
            } => {
                let sk = dugite_crypto::keys::PaymentSigningKey::generate();
                let vk = sk.verification_key();

                let sk_env = serde_json::json!({
                    "type": "StakePoolSigningKey_ed25519",
                    "description": "Stake Pool Operator Cold Signing Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
                });
                let vk_env = serde_json::json!({
                    "type": "StakePoolVerificationKey_ed25519",
                    "description": "Stake Pool Operator Cold Verification Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
                });

                // Counter starts at 0, includes the cold vkey
                let mut counter_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut counter_cbor);
                enc.array(2)?;
                enc.u64(0)?; // counter starts at 0
                enc.bytes(&simple_cbor_wrap(&vk.to_bytes()))?;

                let counter = serde_json::json!({
                    "type": "NodeOperationalCertificateIssueCounter",
                    "description": "Next certificate issue number: 0",
                    "cborHex": hex::encode(&counter_cbor)
                });

                std::fs::write(
                    &cold_signing_key_file,
                    serde_json::to_string_pretty(&sk_env)?,
                )?;
                std::fs::write(
                    &cold_verification_key_file,
                    serde_json::to_string_pretty(&vk_env)?,
                )?;
                std::fs::write(
                    &operational_certificate_counter_file,
                    serde_json::to_string_pretty(&counter)?,
                )?;

                println!("Node cold keys generated.");
                println!(
                    "Cold verification key: {}",
                    cold_verification_key_file.display()
                );
                println!("Cold signing key: {}", cold_signing_key_file.display());
                println!(
                    "Counter: {}",
                    operational_certificate_counter_file.display()
                );
                Ok(())
            }
            NodeSubcommand::KeyGenKes {
                verification_key_file,
                signing_key_file,
            } => {
                // Generate proper Sum6Kes key pair (depth-6 binary sum composition)
                use rand::RngCore;
                let mut seed = [0u8; 32];
                rand::rng().fill_bytes(&mut seed);

                let (sk_bytes, pk_bytes) = dugite_crypto::kes::kes_keygen(&seed)
                    .map_err(|e| anyhow::anyhow!("KES key generation failed: {e}"))?;

                let sk_env = serde_json::json!({
                    "type": "KesSigningKey_ed25519_kes_2^6",
                    "description": "KES Signing Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&sk_bytes))
                });
                let vk_env = serde_json::json!({
                    "type": "KesVerificationKey_ed25519_kes_2^6",
                    "description": "KES Period Verification Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&pk_bytes))
                });

                std::fs::write(&signing_key_file, serde_json::to_string_pretty(&sk_env)?)?;
                std::fs::write(
                    &verification_key_file,
                    serde_json::to_string_pretty(&vk_env)?,
                )?;

                println!("KES key pair generated.");
                println!("Verification key: {}", verification_key_file.display());
                println!("Signing key: {}", signing_key_file.display());
                Ok(())
            }
            NodeSubcommand::KeyGenVrf {
                verification_key_file,
                signing_key_file,
            } => {
                let kp = dugite_crypto::vrf::generate_vrf_keypair();

                let sk_env = serde_json::json!({
                    "type": "VrfSigningKey_PraosVRF",
                    "description": "VRF Signing Key",
                    "cborHex": hex::encode(simple_cbor_wrap(kp.secret_key()))
                });
                let vk_env = serde_json::json!({
                    "type": "VrfVerificationKey_PraosVRF",
                    "description": "VRF Verification Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&kp.public_key))
                });

                std::fs::write(&signing_key_file, serde_json::to_string_pretty(&sk_env)?)?;
                std::fs::write(
                    &verification_key_file,
                    serde_json::to_string_pretty(&vk_env)?,
                )?;

                println!("VRF key pair generated.");
                println!("Verification key: {}", verification_key_file.display());
                println!("Signing key: {}", signing_key_file.display());
                Ok(())
            }
            NodeSubcommand::IssueOpCert {
                kes_verification_key_file,
                cold_signing_key_file,
                operational_certificate_counter_file,
                kes_period,
                out_file,
            } => issue_op_cert(
                &kes_verification_key_file,
                &cold_signing_key_file,
                &operational_certificate_counter_file,
                kes_period,
                &out_file,
            ),
            NodeSubcommand::NewCounter {
                cold_verification_key_file,
                counter_value,
                operational_certificate_counter_file,
            } => {
                let vk_content = std::fs::read_to_string(&cold_verification_key_file)?;
                let vk_env: serde_json::Value = serde_json::from_str(&vk_content)?;
                let vk_cbor_hex = vk_env["cborHex"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing cborHex in cold vkey file"))?;
                let vk_cbor = hex::decode(vk_cbor_hex)?;

                let mut counter_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut counter_cbor);
                enc.array(2)?;
                enc.u64(counter_value)?;
                enc.bytes(&vk_cbor)?;

                let counter_env = serde_json::json!({
                    "type": "NodeOperationalCertificateIssueCounter",
                    "description": format!("Next certificate issue number: {counter_value}"),
                    "cborHex": hex::encode(&counter_cbor)
                });
                std::fs::write(
                    &operational_certificate_counter_file,
                    serde_json::to_string_pretty(&counter_env)?,
                )?;

                println!("Counter created: {counter_value}");
                println!(
                    "Counter file: {}",
                    operational_certificate_counter_file.display()
                );
                Ok(())
            }
            NodeSubcommand::KeyHashVrf {
                verification_key_file,
            } => {
                let content = std::fs::read_to_string(&verification_key_file)?;
                let env: serde_json::Value = serde_json::from_str(&content)?;
                let cbor_hex = env["cborHex"].as_str().ok_or_else(|| {
                    anyhow::anyhow!("Missing cborHex in {}", verification_key_file.display())
                })?;
                let cbor = hex::decode(cbor_hex)?;
                let vrf_key_bytes = if cbor.len() > 2 && cbor[0] == 0x58 {
                    &cbor[2..]
                } else if cbor.len() > 1 && (cbor[0] & 0xe0) == 0x40 {
                    &cbor[1..]
                } else {
                    &cbor
                };
                let hash = dugite_primitives::hash::blake2b_256(vrf_key_bytes);
                println!("{}", hex::encode(hash.as_bytes()));
                Ok(())
            }
        }
    }
}

/// Issue an operational certificate. Shared between `node issue-op-cert` and `stake-pool issue-op-cert`.
pub fn issue_op_cert(
    kes_verification_key_file: &PathBuf,
    cold_signing_key_file: &PathBuf,
    operational_certificate_counter_file: &PathBuf,
    kes_period: u64,
    out_file: &PathBuf,
) -> Result<()> {
    // Read the KES verification key
    let kes_content = std::fs::read_to_string(kes_verification_key_file)?;
    let kes_env: serde_json::Value = serde_json::from_str(&kes_content)?;
    let kes_cbor_hex = kes_env["cborHex"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing cborHex in KES vkey file"))?;
    let kes_cbor = hex::decode(kes_cbor_hex)?;
    let kes_vkey = if kes_cbor.len() > 2 {
        &kes_cbor[2..]
    } else {
        &kes_cbor
    };

    // Read the cold signing key
    let cold_content = std::fs::read_to_string(cold_signing_key_file)?;
    let cold_env: serde_json::Value = serde_json::from_str(&cold_content)?;
    let cold_cbor_hex = cold_env["cborHex"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing cborHex in cold skey file"))?;
    let cold_cbor = hex::decode(cold_cbor_hex)?;
    let cold_key_bytes = if cold_cbor.len() > 2 {
        &cold_cbor[2..]
    } else {
        &cold_cbor
    };
    let cold_sk = dugite_crypto::keys::PaymentSigningKey::from_bytes(cold_key_bytes)?;

    // Read the counter
    let counter_content = std::fs::read_to_string(operational_certificate_counter_file)?;
    let counter_env: serde_json::Value = serde_json::from_str(&counter_content)?;
    let counter_cbor_hex = counter_env["cborHex"].as_str().unwrap_or("8200");
    let counter_cbor = hex::decode(counter_cbor_hex)?;

    // Parse counter value
    let mut decoder = minicbor::Decoder::new(&counter_cbor);
    let _ = decoder.array();
    let counter_value = decoder.u64().unwrap_or(0);

    // Build OCertSignable and sign with the cold key. The byte layout
    // (kes_vkey(32) || seqNo(8 BE) || kesPeriod(8 BE), no CBOR) lives in
    // dugite_crypto::ocert so the verifier and signer share one definition.
    let cert_body = dugite_crypto::ocert::ocert_signable_bytes(kes_vkey, counter_value, kes_period);
    let signature = cold_sk.sign(&cert_body);

    // Build the full operational certificate matching Haskell's OperationalCertificate:
    // array(2) [ocert, cold_vkey]
    // where ocert = array(4) [hot_vkey, sequence_number, kes_period, cold_key_signature]
    let cold_vk = cold_sk.verification_key();
    let mut opcert_cbor = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut opcert_cbor);
    enc.array(2)?;
    // [0] OCert body
    enc.array(4)?;
    enc.bytes(kes_vkey)?;
    enc.u64(counter_value)?;
    enc.u64(kes_period)?;
    enc.bytes(&signature)?;
    // [1] Cold verification key (raw 32 bytes)
    enc.bytes(&cold_vk.to_bytes())?;

    let opcert_env = serde_json::json!({
        "type": "NodeOperationalCertificate",
        "description": "",
        "cborHex": hex::encode(&opcert_cbor)
    });

    std::fs::write(out_file, serde_json::to_string_pretty(&opcert_env)?)?;

    // Increment the counter
    let new_counter = counter_value + 1;
    let mut new_counter_cbor = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut new_counter_cbor);
    enc.array(2)?;
    enc.u64(new_counter)?;
    enc.bytes(&simple_cbor_wrap(&cold_vk.to_bytes()))?;

    let new_counter_env = serde_json::json!({
        "type": "NodeOperationalCertificateIssueCounter",
        "description": format!("Next certificate issue number: {new_counter}"),
        "cborHex": hex::encode(&new_counter_cbor)
    });
    std::fs::write(
        operational_certificate_counter_file,
        serde_json::to_string_pretty(&new_counter_env)?,
    )?;

    println!("Operational certificate issued.");
    println!("Certificate: {}", out_file.display());
    println!("KES period: {kes_period}");
    println!("Counter: {counter_value} -> {new_counter}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── simple_cbor_wrap ────────────────────────────────────────────────────

    #[test]
    fn test_cbor_wrap_tiny() {
        // data.len() < 24 → one-byte header 0x40 | len
        let data = [0u8; 4];
        let wrapped = simple_cbor_wrap(&data);
        assert_eq!(wrapped[0], 0x40 | 4);
        assert_eq!(&wrapped[1..], data.as_slice());
        assert_eq!(wrapped.len(), 5);
    }

    #[test]
    fn test_cbor_wrap_medium() {
        // 24 <= len < 256 → 0x58 prefix + 1-byte length
        let data = vec![0xabu8; 32]; // 32 bytes, a common key length
        let wrapped = simple_cbor_wrap(&data);
        assert_eq!(wrapped[0], 0x58);
        assert_eq!(wrapped[1], 32);
        assert_eq!(&wrapped[2..], data.as_slice());
        assert_eq!(wrapped.len(), 34);
    }

    #[test]
    fn test_cbor_wrap_large() {
        // len >= 256 → 0x59 prefix + 2-byte big-endian length
        let data = vec![0x00u8; 300];
        let wrapped = simple_cbor_wrap(&data);
        assert_eq!(wrapped[0], 0x59);
        let declared_len = u16::from_be_bytes([wrapped[1], wrapped[2]]) as usize;
        assert_eq!(declared_len, 300);
        assert_eq!(&wrapped[3..], data.as_slice());
    }

    #[test]
    fn test_cbor_wrap_empty() {
        // Empty payload: header 0x40, no further bytes
        let wrapped = simple_cbor_wrap(&[]);
        assert_eq!(wrapped, vec![0x40]);
    }

    #[test]
    fn test_cbor_wrap_boundary_23() {
        // Exactly 23 bytes → still tiny path
        let data = vec![0xffu8; 23];
        let wrapped = simple_cbor_wrap(&data);
        assert_eq!(wrapped[0], 0x40 | 23);
        assert_eq!(wrapped.len(), 24);
    }

    #[test]
    fn test_cbor_wrap_boundary_24() {
        // Exactly 24 bytes → medium path (0x58 prefix)
        let data = vec![0xffu8; 24];
        let wrapped = simple_cbor_wrap(&data);
        assert_eq!(wrapped[0], 0x58);
        assert_eq!(wrapped[1], 24);
        assert_eq!(wrapped.len(), 26);
    }

    // ── issue_op_cert via temp files ─────────────────────────────────────────

    /// Generate a cold key pair and return JSON text-envelope strings.
    fn make_cold_key_pair() -> (String, String) {
        let sk = dugite_crypto::keys::PaymentSigningKey::generate();
        let vk = sk.verification_key();
        let sk_json = serde_json::json!({
            "type": "StakePoolSigningKey_ed25519",
            "description": "Stake Pool Operator Signing Key",
            "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
        });
        let vk_json = serde_json::json!({
            "type": "StakePoolVerificationKey_ed25519",
            "description": "Stake Pool Operator Verification Key",
            "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
        });
        (
            serde_json::to_string_pretty(&sk_json).unwrap(),
            serde_json::to_string_pretty(&vk_json).unwrap(),
        )
    }

    /// Generate a KES key pair and return (sk_json, vk_json, pk_bytes).
    fn make_kes_key_pair() -> (String, String, [u8; 32]) {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rng().fill_bytes(&mut seed);
        let (sk_bytes, pk_bytes) = dugite_crypto::kes::kes_keygen(&seed).unwrap();
        let sk_json = serde_json::json!({
            "type": "KesSigningKey_ed25519_kes_2^6",
            "description": "KES Signing Key",
            "cborHex": hex::encode(simple_cbor_wrap(&sk_bytes))
        });
        let vk_json = serde_json::json!({
            "type": "KesVerificationKey_ed25519_kes_2^6",
            "description": "KES Period Verification Key",
            "cborHex": hex::encode(simple_cbor_wrap(&pk_bytes))
        });
        (
            serde_json::to_string_pretty(&sk_json).unwrap(),
            serde_json::to_string_pretty(&vk_json).unwrap(),
            pk_bytes,
        )
    }

    #[test]
    fn test_issue_op_cert_produces_valid_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let cold_sk_path = dir.path().join("cold.skey");
        let cold_vk_path = dir.path().join("cold.vkey");
        let kes_vk_path = dir.path().join("kes.vkey");
        let counter_path = dir.path().join("counter.json");
        let opcert_path = dir.path().join("opcert.json");

        let (cold_sk, _cold_vk) = make_cold_key_pair();
        let (_kes_sk, kes_vk, _kes_pk_bytes) = make_kes_key_pair();

        std::fs::write(&cold_sk_path, &cold_sk).unwrap();
        std::fs::write(&kes_vk_path, &kes_vk).unwrap();
        std::fs::write(&cold_vk_path, &_cold_vk).unwrap();

        // Build a minimal counter file with counter=0 (array(2)[0, bytes(vkey_cbor)])
        // The real issue_op_cert reads: array() → u64 counter → (ignores the rest).
        // We include a placeholder vkey cbor bytes to match the format.
        let placeholder_vkey_cbor = simple_cbor_wrap(&[0u8; 32]);
        let mut counter_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut counter_cbor);
        enc.array(2).unwrap();
        enc.u64(0).unwrap();
        enc.bytes(&placeholder_vkey_cbor).unwrap();
        let cold_vk_env = serde_json::json!({
            "type": "NodeOperationalCertificateIssueCounter",
            "description": "Next certificate issue number: 0",
            "cborHex": hex::encode(&counter_cbor)
        });
        std::fs::write(
            &counter_path,
            serde_json::to_string_pretty(&cold_vk_env).unwrap(),
        )
        .unwrap();

        let result = issue_op_cert(&kes_vk_path, &cold_sk_path, &counter_path, 0, &opcert_path);
        assert!(result.is_ok(), "issue_op_cert failed: {:?}", result.err());

        // Verify the output file is valid JSON with the right type
        let content = std::fs::read_to_string(&opcert_path).unwrap();
        let env: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(env["type"].as_str().unwrap(), "NodeOperationalCertificate");
        assert!(env["cborHex"].as_str().is_some());

        // Verify counter was incremented
        let counter_content = std::fs::read_to_string(&counter_path).unwrap();
        let counter_env: serde_json::Value = serde_json::from_str(&counter_content).unwrap();
        let desc = counter_env["description"].as_str().unwrap();
        assert!(
            desc.contains('1'),
            "counter must be incremented to 1, got: {desc}"
        );
    }

    #[test]
    fn test_issue_op_cert_counter_increments() {
        let dir = tempfile::tempdir().unwrap();
        let cold_sk_path = dir.path().join("cold.skey");
        let kes_vk_path = dir.path().join("kes.vkey");
        let counter_path = dir.path().join("counter.json");
        let opcert_path = dir.path().join("opcert.json");

        let (cold_sk, _) = make_cold_key_pair();
        let (_, kes_vk, _) = make_kes_key_pair();

        std::fs::write(&cold_sk_path, &cold_sk).unwrap();
        std::fs::write(&kes_vk_path, &kes_vk).unwrap();

        // Counter starts at 5 — same format as issue_op_cert expects
        let placeholder_vkey_cbor = simple_cbor_wrap(&[0u8; 32]);
        let mut counter_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut counter_cbor);
        enc.array(2).unwrap();
        enc.u64(5).unwrap();
        enc.bytes(&placeholder_vkey_cbor).unwrap();
        let counter_env = serde_json::json!({
            "type": "NodeOperationalCertificateIssueCounter",
            "description": "Next certificate issue number: 5",
            "cborHex": hex::encode(&counter_cbor)
        });
        std::fs::write(
            &counter_path,
            serde_json::to_string_pretty(&counter_env).unwrap(),
        )
        .unwrap();

        issue_op_cert(&kes_vk_path, &cold_sk_path, &counter_path, 42, &opcert_path).unwrap();

        let counter_content = std::fs::read_to_string(&counter_path).unwrap();
        let updated: serde_json::Value = serde_json::from_str(&counter_content).unwrap();
        assert!(
            updated["description"].as_str().unwrap().contains('6'),
            "counter must advance from 5 to 6"
        );
    }

    // ── issue_op_cert: certificate structure + signature ─────────────────────

    /// The issued opcert must decode as array(2)[array(4)[kes_vkey, seq, period,
    /// sig], cold_vkey] and the signature must verify with the cold key over
    /// the shared OCertSignable byte layout. This is what a Haskell node checks
    /// when the certificate is presented in a block header.
    #[test]
    fn test_issue_op_cert_cbor_structure_and_signature() {
        let dir = tempfile::tempdir().unwrap();
        let cold_sk_path = dir.path().join("cold.skey");
        let kes_vk_path = dir.path().join("kes.vkey");
        let counter_path = dir.path().join("counter.json");
        let opcert_path = dir.path().join("opcert.json");

        let cold_sk = dugite_crypto::keys::PaymentSigningKey::generate();
        let cold_vk = cold_sk.verification_key();
        let sk_json = serde_json::json!({
            "type": "StakePoolSigningKey_ed25519",
            "description": "",
            "cborHex": hex::encode(simple_cbor_wrap(&cold_sk.to_bytes()))
        });
        std::fs::write(&cold_sk_path, serde_json::to_string(&sk_json).unwrap()).unwrap();

        let (_, kes_vk_json, kes_pk_bytes) = make_kes_key_pair();
        std::fs::write(&kes_vk_path, &kes_vk_json).unwrap();

        let mut counter_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut counter_cbor);
        enc.array(2).unwrap();
        enc.u64(3).unwrap();
        enc.bytes(&simple_cbor_wrap(&cold_vk.to_bytes())).unwrap();
        let counter_env = serde_json::json!({
            "type": "NodeOperationalCertificateIssueCounter",
            "description": "Next certificate issue number: 3",
            "cborHex": hex::encode(&counter_cbor)
        });
        std::fs::write(&counter_path, serde_json::to_string(&counter_env).unwrap()).unwrap();

        issue_op_cert(&kes_vk_path, &cold_sk_path, &counter_path, 17, &opcert_path).unwrap();

        let env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&opcert_path).unwrap()).unwrap();
        let opcert_cbor = hex::decode(env["cborHex"].as_str().unwrap()).unwrap();

        let mut d = minicbor::Decoder::new(&opcert_cbor);
        assert_eq!(d.array().unwrap(), Some(2), "opcert must be array(2)");
        assert_eq!(d.array().unwrap(), Some(4), "ocert body must be array(4)");
        let hot_vkey = d.bytes().unwrap();
        assert_eq!(
            hot_vkey,
            kes_pk_bytes.as_slice(),
            "hot vkey must be the KES vkey"
        );
        let seq = d.u64().unwrap();
        assert_eq!(seq, 3, "sequence number must be the pre-increment counter");
        let period = d.u64().unwrap();
        assert_eq!(period, 17);
        let sig = d.bytes().unwrap().to_vec();
        assert_eq!(sig.len(), 64, "Ed25519 signature is 64 bytes");
        let embedded_cold_vk = d.bytes().unwrap();
        assert_eq!(embedded_cold_vk, cold_vk.to_bytes().as_slice());

        // Signature must verify over the canonical OCertSignable layout.
        let signable = dugite_crypto::ocert::ocert_signable_bytes(&kes_pk_bytes, 3, 17);
        cold_vk
            .verify(&signable, &sig)
            .expect("opcert signature must verify with the cold verification key");
    }

    // ── issue_op_cert error paths ────────────────────────────────────────────

    #[test]
    fn test_issue_op_cert_missing_kes_cbor_hex_errors() {
        let dir = tempfile::tempdir().unwrap();
        let kes_vk_path = dir.path().join("kes.vkey");
        let cold_sk_path = dir.path().join("cold.skey");
        let counter_path = dir.path().join("counter.json");
        let opcert_path = dir.path().join("opcert.json");

        // KES envelope with no cborHex field at all.
        std::fs::write(
            &kes_vk_path,
            r#"{"type": "KesVerificationKey_ed25519_kes_2^6"}"#,
        )
        .unwrap();
        let (cold_sk, _) = make_cold_key_pair();
        std::fs::write(&cold_sk_path, &cold_sk).unwrap();
        std::fs::write(&counter_path, r#"{"cborHex": "8200"}"#).unwrap();

        let result = issue_op_cert(&kes_vk_path, &cold_sk_path, &counter_path, 0, &opcert_path);
        assert!(result.is_err(), "missing cborHex in KES vkey must error");
        assert!(
            !opcert_path.exists(),
            "no certificate may be written on failure"
        );
    }

    #[test]
    fn test_issue_op_cert_nonexistent_counter_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let kes_vk_path = dir.path().join("kes.vkey");
        let cold_sk_path = dir.path().join("cold.skey");
        let opcert_path = dir.path().join("opcert.json");

        let (cold_sk, _) = make_cold_key_pair();
        let (_, kes_vk, _) = make_kes_key_pair();
        std::fs::write(&cold_sk_path, &cold_sk).unwrap();
        std::fs::write(&kes_vk_path, &kes_vk).unwrap();

        let result = issue_op_cert(
            &kes_vk_path,
            &cold_sk_path,
            &dir.path().join("missing-counter.json"),
            0,
            &opcert_path,
        );
        assert!(result.is_err(), "nonexistent counter file must error");
    }

    #[test]
    fn test_issue_op_cert_truncated_cold_key_errors() {
        let dir = tempfile::tempdir().unwrap();
        let kes_vk_path = dir.path().join("kes.vkey");
        let cold_sk_path = dir.path().join("cold.skey");
        let counter_path = dir.path().join("counter.json");
        let opcert_path = dir.path().join("opcert.json");

        let (_, kes_vk, _) = make_kes_key_pair();
        std::fs::write(&kes_vk_path, &kes_vk).unwrap();
        // 16-byte cold key payload — invalid for Ed25519.
        let bad_sk = serde_json::json!({
            "type": "StakePoolSigningKey_ed25519",
            "description": "",
            "cborHex": format!("5810{}", hex::encode([0u8; 16]))
        });
        std::fs::write(&cold_sk_path, serde_json::to_string(&bad_sk).unwrap()).unwrap();
        std::fs::write(&counter_path, r#"{"cborHex": "8200"}"#).unwrap();

        let result = issue_op_cert(&kes_vk_path, &cold_sk_path, &counter_path, 0, &opcert_path);
        assert!(result.is_err(), "truncated cold key must error");
    }

    // ── NodeCmd::run key generation paths ────────────────────────────────────

    #[test]
    fn test_run_key_gen_writes_cold_keys_and_counter() {
        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("cold.vkey");
        let sk_path = dir.path().join("cold.skey");
        let counter_path = dir.path().join("counter.json");

        NodeCmd {
            command: NodeSubcommand::KeyGen {
                cold_verification_key_file: vk_path.clone(),
                cold_signing_key_file: sk_path.clone(),
                operational_certificate_counter_file: counter_path.clone(),
            },
        }
        .run()
        .unwrap();

        let vk_env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&vk_path).unwrap()).unwrap();
        let sk_env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sk_path).unwrap()).unwrap();
        assert_eq!(vk_env["type"], "StakePoolVerificationKey_ed25519");
        assert_eq!(sk_env["type"], "StakePoolSigningKey_ed25519");

        // The written signing key must derive the written verification key.
        let sk_cbor = hex::decode(sk_env["cborHex"].as_str().unwrap()).unwrap();
        let vk_cbor = hex::decode(vk_env["cborHex"].as_str().unwrap()).unwrap();
        let sk = dugite_crypto::keys::PaymentSigningKey::from_bytes(&sk_cbor[2..]).unwrap();
        assert_eq!(
            sk.verification_key().to_bytes().as_slice(),
            &vk_cbor[2..],
            "cold vkey file must match the cold skey file"
        );

        // Counter: array(2)[0, bytes(cbor-wrapped vkey)] with the vkey embedded.
        let counter_env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&counter_path).unwrap()).unwrap();
        assert_eq!(
            counter_env["type"],
            "NodeOperationalCertificateIssueCounter"
        );
        let counter_cbor = hex::decode(counter_env["cborHex"].as_str().unwrap()).unwrap();
        let mut d = minicbor::Decoder::new(&counter_cbor);
        assert_eq!(d.array().unwrap(), Some(2));
        assert_eq!(d.u64().unwrap(), 0, "fresh counter must start at 0");
        assert_eq!(
            d.bytes().unwrap(),
            vk_cbor.as_slice(),
            "counter must embed the cold vkey CBOR"
        );
    }

    #[test]
    fn test_run_key_gen_kes_envelope_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("kes.vkey");
        let sk_path = dir.path().join("kes.skey");

        NodeCmd {
            command: NodeSubcommand::KeyGenKes {
                verification_key_file: vk_path.clone(),
                signing_key_file: sk_path.clone(),
            },
        }
        .run()
        .unwrap();

        let vk_env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&vk_path).unwrap()).unwrap();
        let sk_env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sk_path).unwrap()).unwrap();
        assert_eq!(vk_env["type"], "KesVerificationKey_ed25519_kes_2^6");
        assert_eq!(sk_env["type"], "KesSigningKey_ed25519_kes_2^6");
        // vkey: 32 bytes → 5820; skey: 612 bytes (Sum6Kes) → 590264.
        assert!(vk_env["cborHex"].as_str().unwrap().starts_with("5820"));
        assert!(sk_env["cborHex"].as_str().unwrap().starts_with("590264"));
    }

    #[test]
    fn test_run_key_gen_vrf_envelope_prefixes() {
        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("vrf.vkey");
        let sk_path = dir.path().join("vrf.skey");

        NodeCmd {
            command: NodeSubcommand::KeyGenVrf {
                verification_key_file: vk_path.clone(),
                signing_key_file: sk_path.clone(),
            },
        }
        .run()
        .unwrap();

        let vk_env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&vk_path).unwrap()).unwrap();
        let sk_env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&sk_path).unwrap()).unwrap();
        assert_eq!(vk_env["type"], "VrfVerificationKey_PraosVRF");
        assert_eq!(sk_env["type"], "VrfSigningKey_PraosVRF");
        assert!(vk_env["cborHex"].as_str().unwrap().starts_with("5820"));
        assert!(sk_env["cborHex"].as_str().unwrap().starts_with("5820"));
    }

    // ── NewCounter ───────────────────────────────────────────────────────────

    #[test]
    fn test_run_new_counter_writes_requested_value() {
        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("cold.vkey");
        let counter_path = dir.path().join("counter.json");

        let (_, cold_vk) = make_cold_key_pair();
        std::fs::write(&vk_path, &cold_vk).unwrap();

        NodeCmd {
            command: NodeSubcommand::NewCounter {
                cold_verification_key_file: vk_path.clone(),
                counter_value: 42,
                operational_certificate_counter_file: counter_path.clone(),
            },
        }
        .run()
        .unwrap();

        let env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&counter_path).unwrap()).unwrap();
        assert_eq!(env["type"], "NodeOperationalCertificateIssueCounter");
        assert_eq!(
            env["description"].as_str().unwrap(),
            "Next certificate issue number: 42"
        );

        let cbor = hex::decode(env["cborHex"].as_str().unwrap()).unwrap();
        let mut d = minicbor::Decoder::new(&cbor);
        assert_eq!(d.array().unwrap(), Some(2));
        assert_eq!(d.u64().unwrap(), 42, "counter CBOR must carry the value");
        // The embedded bytes must be the vkey file's cborHex bytes.
        let vk_env: serde_json::Value = serde_json::from_str(&cold_vk).unwrap();
        let vk_cbor = hex::decode(vk_env["cborHex"].as_str().unwrap()).unwrap();
        assert_eq!(d.bytes().unwrap(), vk_cbor.as_slice());
    }

    #[test]
    fn test_run_new_counter_missing_cbor_hex_errors() {
        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("cold.vkey");
        let counter_path = dir.path().join("counter.json");
        std::fs::write(&vk_path, r#"{"type": "StakePoolVerificationKey_ed25519"}"#).unwrap();

        let result = NodeCmd {
            command: NodeSubcommand::NewCounter {
                cold_verification_key_file: vk_path,
                counter_value: 1,
                operational_certificate_counter_file: counter_path.clone(),
            },
        }
        .run();
        assert!(result.is_err(), "missing cborHex must error");
        assert!(!counter_path.exists(), "no counter file on failure");
    }

    // ── KeyHashVrf error paths ───────────────────────────────────────────────

    #[test]
    fn test_run_key_hash_vrf_missing_cbor_hex_errors() {
        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("vrf.vkey");
        std::fs::write(&vk_path, r#"{"type": "VrfVerificationKey_PraosVRF"}"#).unwrap();

        let result = NodeCmd {
            command: NodeSubcommand::KeyHashVrf {
                verification_key_file: vk_path,
            },
        }
        .run();
        assert!(result.is_err(), "missing cborHex must error");
    }

    #[test]
    fn test_run_key_hash_vrf_bad_hex_errors() {
        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("vrf.vkey");
        std::fs::write(&vk_path, r#"{"cborHex": "xyz"}"#).unwrap();

        let result = NodeCmd {
            command: NodeSubcommand::KeyHashVrf {
                verification_key_file: vk_path,
            },
        }
        .run();
        assert!(result.is_err(), "non-hex cborHex must error");
    }
}
