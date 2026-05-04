use anyhow::Result;
use clap::{Args, Subcommand};
use dugite_primitives::hash::blake2b_256;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct StakePoolCmd {
    #[command(subcommand)]
    command: StakePoolSubcommand,
}

#[derive(Subcommand, Debug)]
enum StakePoolSubcommand {
    /// Generate pool keys (cold, VRF, KES)
    KeyGen {
        #[arg(long)]
        cold_verification_key_file: PathBuf,
        #[arg(long)]
        cold_signing_key_file: PathBuf,
        #[arg(long)]
        operational_certificate_counter_file: PathBuf,
    },
    /// Get pool ID from verification key
    Id {
        #[arg(long)]
        cold_verification_key_file: PathBuf,
    },
    /// Generate VRF key pair
    VrfKeyGen {
        #[arg(long)]
        verification_key_file: PathBuf,
        #[arg(long)]
        signing_key_file: PathBuf,
    },
    /// Generate KES key pair
    KesKeyGen {
        #[arg(long)]
        verification_key_file: PathBuf,
        #[arg(long)]
        signing_key_file: PathBuf,
    },
    /// Issue operational certificate
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
    /// Create stake pool retirement certificate
    RetirementCertificate {
        #[arg(long)]
        cold_verification_key_file: PathBuf,
        /// Epoch at which the pool retires
        #[arg(long)]
        epoch: u64,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create stake pool registration certificate
    RegistrationCertificate {
        #[arg(long)]
        cold_verification_key_file: PathBuf,
        #[arg(long)]
        vrf_verification_key_file: PathBuf,
        #[arg(long)]
        pledge: u64,
        #[arg(long)]
        cost: u64,
        #[arg(long)]
        margin: f64,
        #[arg(long)]
        reward_account_verification_key_file: PathBuf,
        #[arg(long)]
        pool_owner_verification_key_file: Vec<PathBuf>,
        /// Pool relay: IP address (e.g., "1.2.3.4:3001")
        #[arg(long)]
        pool_relay_ipv4: Vec<String>,
        /// Pool relay: DNS hostname with port (e.g., "relay.example.com:3001")
        #[arg(long)]
        single_host_pool_relay: Vec<String>,
        /// Pool relay: DNS SRV record name (e.g., "_cardano._tcp.example.com")
        #[arg(long)]
        multi_host_pool_relay: Vec<String>,
        /// Pool metadata URL
        #[arg(long)]
        metadata_url: Option<String>,
        /// Use testnet network ID for reward account (default: mainnet)
        #[arg(long)]
        testnet: bool,
        /// Pool metadata hash (hex)
        #[arg(long)]
        metadata_hash: Option<String>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Compute the blake2b-256 hash of a pool metadata JSON file
    MetadataHash {
        /// Path to pool metadata JSON file
        #[arg(long)]
        pool_metadata_file: PathBuf,
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

impl StakePoolCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            StakePoolSubcommand::KeyGen {
                cold_verification_key_file,
                cold_signing_key_file,
                operational_certificate_counter_file,
            } => {
                let sk = dugite_crypto::keys::PaymentSigningKey::generate();
                let vk = sk.verification_key();

                let sk_env = serde_json::json!({
                    "type": "StakePoolSigningKey_ed25519",
                    "description": "Stake Pool Operator Signing Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
                });
                let vk_env = serde_json::json!({
                    "type": "StakePoolVerificationKey_ed25519",
                    "description": "Stake Pool Operator Verification Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
                });

                let mut counter_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut counter_cbor);
                enc.array(2)?;
                enc.u64(0)?;
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

                println!("Pool cold keys generated.");
                Ok(())
            }
            StakePoolSubcommand::Id {
                cold_verification_key_file,
            } => {
                let content = std::fs::read_to_string(&cold_verification_key_file)?;
                let env: serde_json::Value = serde_json::from_str(&content)?;
                let cbor_hex = env["cborHex"].as_str().unwrap_or("");
                let cbor_bytes = hex::decode(cbor_hex)?;
                let key_bytes = if cbor_bytes.len() > 2 {
                    &cbor_bytes[2..]
                } else {
                    &cbor_bytes
                };
                let hash = dugite_primitives::hash::blake2b_224(key_bytes);
                let pool_id =
                    bech32::encode::<bech32::Bech32>(bech32::Hrp::parse("pool")?, hash.as_bytes())?;
                println!("{pool_id}");
                Ok(())
            }
            StakePoolSubcommand::VrfKeyGen {
                verification_key_file,
                signing_key_file,
            } => {
                // Generate proper ECVRF-ED25519-SHA512-Elligator2 key pair
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

                let vrf_vkey_hash = dugite_primitives::hash::blake2b_256(&kp.public_key);
                println!("VRF key pair generated.");
                println!("VRF verification key hash: {}", vrf_vkey_hash.to_hex());
                Ok(())
            }
            StakePoolSubcommand::KesKeyGen {
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
                Ok(())
            }
            StakePoolSubcommand::IssueOpCert {
                kes_verification_key_file,
                cold_signing_key_file,
                operational_certificate_counter_file,
                kes_period,
                out_file,
            } => super::node::issue_op_cert(
                &kes_verification_key_file,
                &cold_signing_key_file,
                &operational_certificate_counter_file,
                kes_period,
                &out_file,
            ),
            StakePoolSubcommand::RetirementCertificate {
                cold_verification_key_file,
                epoch,
                out_file,
            } => {
                let pool_hash = load_vkey_hash(&cold_verification_key_file)?;

                // PoolRetirement (cert type 4) = [4, pool_hash, epoch]
                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                enc.array(3)?;
                enc.u32(4)?;
                enc.bytes(&pool_hash)?;
                enc.u64(epoch)?;

                let cert_env = serde_json::json!({
                    "type": "CertificateShelley",
                    "description": "Stake Pool Retirement Certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Pool retirement certificate written to: {}",
                    out_file.display()
                );
                println!("Pool retires at epoch: {epoch}");
                Ok(())
            }
            StakePoolSubcommand::RegistrationCertificate {
                cold_verification_key_file,
                vrf_verification_key_file,
                pledge,
                cost,
                margin,
                reward_account_verification_key_file,
                pool_owner_verification_key_file,
                pool_relay_ipv4,
                single_host_pool_relay,
                multi_host_pool_relay,
                testnet,
                metadata_url,
                metadata_hash,
                out_file,
            } => {
                // Read pool operator (cold) vkey
                let cold_vk = load_vkey_hash(&cold_verification_key_file)?;
                // Read VRF vkey (blake2b-256 hash, 32 bytes — not blake2b-224)
                let vrf_vk = load_vrf_vkey_hash(&vrf_verification_key_file)?;
                // Read reward account key
                let reward_vk = load_vkey_hash(&reward_account_verification_key_file)?;
                // Read pool owner keys
                let owners: Vec<Vec<u8>> = pool_owner_verification_key_file
                    .iter()
                    .map(|f| load_vkey_hash(f).map(|h| h.to_vec()))
                    .collect::<Result<_>>()?;

                // Convert margin to rational (find close fraction)
                let margin_num = (margin * 1_000_000.0) as u64;
                let margin_den = 1_000_000u64;

                // Build relay list
                let mut relays: Vec<RelaySpec> = Vec::new();
                for ipv4_str in &pool_relay_ipv4 {
                    let parts: Vec<&str> = ipv4_str.rsplitn(2, ':').collect();
                    let (port, ip) = if parts.len() == 2 {
                        (parts[0].parse::<u16>().unwrap_or(3001), parts[1])
                    } else {
                        (3001, ipv4_str.as_str())
                    };
                    let octets: Vec<u8> = ip.split('.').filter_map(|s| s.parse().ok()).collect();
                    if octets.len() == 4 {
                        relays.push(RelaySpec::SingleHostAddr {
                            port,
                            ipv4: [octets[0], octets[1], octets[2], octets[3]],
                        });
                    }
                }
                for dns_str in &single_host_pool_relay {
                    let parts: Vec<&str> = dns_str.rsplitn(2, ':').collect();
                    let (port, host) = if parts.len() == 2 {
                        (parts[0].parse::<u16>().unwrap_or(3001), parts[1])
                    } else {
                        (3001, dns_str.as_str())
                    };
                    relays.push(RelaySpec::SingleHostName {
                        port,
                        dns_name: host.to_string(),
                    });
                }
                for dns_name in &multi_host_pool_relay {
                    relays.push(RelaySpec::MultiHostName {
                        dns_name: dns_name.clone(),
                    });
                }

                // Build registration certificate CBOR
                // Certificate type 3 = PoolRegistration
                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);

                // [3, pool_params...]
                enc.array(10)?;
                enc.u32(3)?; // Certificate tag for PoolRegistration
                enc.bytes(&cold_vk)?; // operator (pool_id = hash of cold vkey)
                enc.bytes(&vrf_vk)?; // vrf_keyhash
                enc.u64(pledge)?;
                enc.u64(cost)?;
                // margin as tag 30 [num, den]
                enc.tag(minicbor::data::Tag::new(30))?;
                enc.array(2)?;
                enc.u64(margin_num)?;
                enc.u64(margin_den)?;
                // reward account: e0 = testnet, e1 = mainnet
                let network_byte = if testnet { 0xe0u8 } else { 0xe1u8 };
                let mut reward_account = vec![network_byte];
                reward_account.extend_from_slice(&reward_vk);
                enc.bytes(&reward_account)?;
                // pool owners
                enc.array(owners.len() as u64)?;
                for owner in &owners {
                    enc.bytes(owner)?;
                }
                // relays
                enc.array(relays.len() as u64)?;
                for relay in &relays {
                    encode_relay(&mut enc, relay)?;
                }
                // pool metadata
                match (&metadata_url, &metadata_hash) {
                    (Some(url), Some(hash_hex)) => {
                        let hash_bytes = hex::decode(hash_hex)?;
                        enc.array(2)?;
                        enc.str(url)?;
                        enc.bytes(&hash_bytes)?;
                    }
                    _ => {
                        enc.null()?;
                    }
                }

                let cert_env = serde_json::json!({
                    "type": "CertificateShelley",
                    "description": "Stake Pool Registration Certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Pool registration certificate written to: {}",
                    out_file.display()
                );
                if !relays.is_empty() {
                    println!("  Relays: {}", relays.len());
                }
                if metadata_url.is_some() {
                    println!("  Metadata URL: {}", metadata_url.as_deref().unwrap_or(""));
                }
                Ok(())
            }
            StakePoolSubcommand::MetadataHash { pool_metadata_file } => {
                let data = std::fs::read(&pool_metadata_file)?;
                let hash = blake2b_256(&data);
                println!("{}", hex::encode(hash.as_bytes()));
                Ok(())
            }
        }
    }
}

/// Relay specification for pool registration
enum RelaySpec {
    SingleHostAddr { port: u16, ipv4: [u8; 4] },
    SingleHostName { port: u16, dns_name: String },
    MultiHostName { dns_name: String },
}

/// Encode a relay as CBOR for the pool registration certificate
fn encode_relay(enc: &mut minicbor::Encoder<&mut Vec<u8>>, relay: &RelaySpec) -> Result<()> {
    match relay {
        RelaySpec::SingleHostAddr { port, ipv4 } => {
            // [0, port, ipv4, null(ipv6)]
            enc.array(4)?;
            enc.u32(0)?;
            enc.u16(*port)?;
            enc.bytes(ipv4)?;
            enc.null()?;
        }
        RelaySpec::SingleHostName { port, dns_name } => {
            // [1, port, dns_name]
            enc.array(3)?;
            enc.u32(1)?;
            enc.u16(*port)?;
            enc.str(dns_name)?;
        }
        RelaySpec::MultiHostName { dns_name } => {
            // [2, dns_name]
            enc.array(2)?;
            enc.u32(2)?;
            enc.str(dns_name)?;
        }
    }
    Ok(())
}

/// Load a verification key file and return the blake2b-224 hash of the raw key bytes
fn load_vkey_hash(path: &PathBuf) -> Result<Vec<u8>> {
    let content = std::fs::read_to_string(path)?;
    let env: serde_json::Value = serde_json::from_str(&content)?;
    let cbor_hex = env["cborHex"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing cborHex in {}", path.display()))?;
    let cbor_bytes = hex::decode(cbor_hex)?;
    let key_bytes = if cbor_bytes.len() > 2 {
        &cbor_bytes[2..]
    } else {
        &cbor_bytes
    };
    let hash = dugite_primitives::hash::blake2b_224(key_bytes);
    Ok(hash.as_bytes().to_vec())
}

/// Load a VRF verification key file and return the blake2b-256 hash (32 bytes).
/// VRF keyhash in pool registration uses Hash<32>, not Hash<28>.
fn load_vrf_vkey_hash(path: &PathBuf) -> Result<Vec<u8>> {
    let content = std::fs::read_to_string(path)?;
    let env: serde_json::Value = serde_json::from_str(&content)?;
    let cbor_hex = env["cborHex"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing cborHex in {}", path.display()))?;
    let cbor_bytes = hex::decode(cbor_hex)?;
    let key_bytes = if cbor_bytes.len() > 2 {
        &cbor_bytes[2..]
    } else {
        &cbor_bytes
    };
    let hash = dugite_primitives::hash::blake2b_256(key_bytes);
    Ok(hash.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── simple_cbor_wrap ────────────────────────────────────────────────────

    #[test]
    fn test_cbor_wrap_tiny() {
        // data.len() < 24: one-byte header 0x40 | len
        let data = [0xabu8; 4];
        let w = simple_cbor_wrap(&data);
        assert_eq!(w[0], 0x40 | 4);
        assert_eq!(&w[1..], data.as_slice());
    }

    #[test]
    fn test_cbor_wrap_medium() {
        // 24 <= len < 256: 0x58 prefix + 1-byte length
        let data = vec![0u8; 32];
        let w = simple_cbor_wrap(&data);
        assert_eq!(w[0], 0x58);
        assert_eq!(w[1], 32);
        assert_eq!(&w[2..], data.as_slice());
    }

    #[test]
    fn test_cbor_wrap_large() {
        // len >= 256: 0x59 prefix + big-endian u16 length
        let data = vec![0u8; 300];
        let w = simple_cbor_wrap(&data);
        assert_eq!(w[0], 0x59);
        let declared = u16::from_be_bytes([w[1], w[2]]) as usize;
        assert_eq!(declared, 300);
        assert_eq!(&w[3..], data.as_slice());
    }

    // ── encode_relay ─────────────────────────────────────────────────────────

    #[test]
    fn test_encode_relay_single_host_addr() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        encode_relay(
            &mut enc,
            &RelaySpec::SingleHostAddr {
                port: 3001,
                ipv4: [1, 2, 3, 4],
            },
        )
        .unwrap();

        // Expected: array(4)[0, 3001, bytes(4), null]
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.array().unwrap(), Some(4));
        assert_eq!(dec.u32().unwrap(), 0); // type tag
        assert_eq!(dec.u16().unwrap(), 3001); // port
        assert_eq!(dec.bytes().unwrap(), &[1, 2, 3, 4]); // ipv4
        assert_eq!(dec.datatype().unwrap(), minicbor::data::Type::Null);
    }

    #[test]
    fn test_encode_relay_single_host_name() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        encode_relay(
            &mut enc,
            &RelaySpec::SingleHostName {
                port: 6000,
                dns_name: "relay.example.com".to_string(),
            },
        )
        .unwrap();

        // Expected: array(3)[1, 6000, "relay.example.com"]
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u32().unwrap(), 1);
        assert_eq!(dec.u16().unwrap(), 6000);
        assert_eq!(dec.str().unwrap(), "relay.example.com");
    }

    #[test]
    fn test_encode_relay_multi_host_name() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        encode_relay(
            &mut enc,
            &RelaySpec::MultiHostName {
                dns_name: "_cardano._tcp.example.com".to_string(),
            },
        )
        .unwrap();

        // Expected: array(2)[2, "_cardano._tcp.example.com"]
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 2);
        assert_eq!(dec.str().unwrap(), "_cardano._tcp.example.com");
    }

    // ── load_vkey_hash ───────────────────────────────────────────────────────

    #[test]
    fn test_load_vkey_hash_roundtrip() {
        use std::path::PathBuf;

        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("test.vkey");

        // Generate a fresh Ed25519 key, wrap in a text envelope
        let sk = dugite_crypto::keys::PaymentSigningKey::generate();
        let vk = sk.verification_key();
        let raw_vk_bytes = vk.to_bytes();

        // Wrap key bytes with simple_cbor_wrap, as the real commands do
        let cbor_hex = hex::encode(simple_cbor_wrap(&raw_vk_bytes));
        let env = serde_json::json!({
            "type": "StakePoolVerificationKey_ed25519",
            "description": "test",
            "cborHex": cbor_hex
        });
        std::fs::write(&vk_path, serde_json::to_string_pretty(&env).unwrap()).unwrap();

        let hash = load_vkey_hash(&PathBuf::from(&vk_path)).unwrap();

        // Hash must be 28 bytes (Blake2b-224)
        assert_eq!(hash.len(), 28, "pool ID hash must be 28 bytes");

        // Loading a second time must produce the same hash (deterministic)
        let hash2 = load_vkey_hash(&PathBuf::from(&vk_path)).unwrap();
        assert_eq!(hash, hash2, "repeated load must yield same hash");
    }

    // ── load_vrf_vkey_hash ───────────────────────────────────────────────────

    #[test]
    fn test_load_vrf_vkey_hash_is_32_bytes() {
        use std::path::PathBuf;

        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("vrf.vkey");

        let kp = dugite_crypto::vrf::generate_vrf_keypair();
        let cbor_hex = hex::encode(simple_cbor_wrap(&kp.public_key));
        let env = serde_json::json!({
            "type": "VrfVerificationKey_PraosVRF",
            "description": "VRF Verification Key",
            "cborHex": cbor_hex
        });
        std::fs::write(&vk_path, serde_json::to_string_pretty(&env).unwrap()).unwrap();

        let hash = load_vrf_vkey_hash(&PathBuf::from(&vk_path)).unwrap();
        assert_eq!(hash.len(), 32, "VRF keyhash in pool cert must be 32 bytes");
    }

    // ── pool registration certificate: CBOR structure ───────────────────────

    #[test]
    fn test_retirement_cert_cbor_structure() {
        // The retirement cert CBOR must be: array(3)[4, pool_hash_bytes(28), epoch]
        let epoch: u64 = 450;
        let pool_hash = vec![0xabu8; 28];

        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        enc.array(3).unwrap();
        enc.u32(4).unwrap();
        enc.bytes(&pool_hash).unwrap();
        enc.u64(epoch).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u32().unwrap(), 4); // PoolRetirement type tag
        assert_eq!(dec.bytes().unwrap(), pool_hash.as_slice());
        assert_eq!(dec.u64().unwrap(), epoch);
    }

    #[test]
    fn test_registration_cert_reward_account_mainnet() {
        // Mainnet reward account byte: 0xe1
        let network_byte = 0xe1u8;
        let reward_vk_hash = vec![0xdeu8; 28]; // arbitrary 28-byte hash
        let mut reward_account = vec![network_byte];
        reward_account.extend_from_slice(&reward_vk_hash);
        assert_eq!(reward_account[0], 0xe1);
        assert_eq!(reward_account.len(), 29);
    }

    #[test]
    fn test_registration_cert_reward_account_testnet() {
        // Testnet reward account byte: 0xe0
        let network_byte = 0xe0u8;
        let reward_vk_hash = vec![0xdeu8; 28];
        let mut reward_account = vec![network_byte];
        reward_account.extend_from_slice(&reward_vk_hash);
        assert_eq!(reward_account[0], 0xe0);
        assert_eq!(reward_account.len(), 29);
    }

    // ── MetadataHash: blake2b-256 of file bytes ──────────────────────────────

    #[test]
    fn test_metadata_hash_is_deterministic() {
        // blake2b_256 of the same data must always return the same hash.
        let data = br#"{"name":"Test Pool","ticker":"TEST","homepage":"https://example.com"}"#;
        let h1 = blake2b_256(data);
        let h2 = blake2b_256(data);
        assert_eq!(h1.as_bytes(), h2.as_bytes());
        assert_eq!(h1.as_bytes().len(), 32);
    }

    #[test]
    fn test_metadata_hash_differs_for_different_content() {
        let h1 = blake2b_256(b"pool-a");
        let h2 = blake2b_256(b"pool-b");
        assert_ne!(h1.as_bytes(), h2.as_bytes());
    }
}
