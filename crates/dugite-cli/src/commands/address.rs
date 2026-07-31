use anyhow::Result;
use clap::{Args, Subcommand};
use dugite_crypto::keys::{PaymentVerificationKey, TextEnvelope};
use dugite_primitives::address::{Address, BaseAddress, EnterpriseAddress};
use dugite_primitives::credentials::Credential;
use dugite_primitives::network::NetworkId;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct AddressCmd {
    #[command(subcommand)]
    command: AddressSubcommand,
}

#[derive(Subcommand, Debug)]
enum AddressSubcommand {
    /// Generate a payment key pair
    KeyGen {
        /// Output verification key file
        #[arg(long)]
        verification_key_file: PathBuf,
        /// Output signing key file
        #[arg(long)]
        signing_key_file: PathBuf,
    },
    /// Get the hash of a verification key
    KeyHash {
        /// Payment verification key file
        #[arg(long)]
        payment_verification_key_file: PathBuf,
    },
    /// Build an address from verification keys
    Build {
        /// Payment verification key file
        #[arg(long)]
        payment_verification_key_file: PathBuf,
        /// Stake verification key file (optional - creates base address if provided)
        #[arg(long)]
        stake_verification_key_file: Option<PathBuf>,
        /// Network (mainnet or testnet)
        #[arg(long, default_value = "mainnet")]
        network: String,
        /// Output file (prints to stdout if not provided)
        #[arg(long)]
        out_file: Option<PathBuf>,
    },
    /// Show address info
    Info {
        /// Bech32 address
        #[arg(long)]
        address: String,
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

impl AddressCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            AddressSubcommand::KeyGen {
                verification_key_file,
                signing_key_file,
            } => {
                let sk = dugite_crypto::keys::PaymentSigningKey::generate();
                let vk = sk.verification_key();

                let sk_env = serde_json::json!({
                    "type": "PaymentSigningKeyShelley_ed25519",
                    "description": "Payment Signing Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
                });
                let vk_env = serde_json::json!({
                    "type": "PaymentVerificationKeyShelley_ed25519",
                    "description": "Payment Verification Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
                });

                std::fs::write(&signing_key_file, serde_json::to_string_pretty(&sk_env)?)?;
                std::fs::write(
                    &verification_key_file,
                    serde_json::to_string_pretty(&vk_env)?,
                )?;

                println!("Payment key pair generated.");
                println!("Verification key: {}", verification_key_file.display());
                println!("Signing key: {}", signing_key_file.display());
                Ok(())
            }
            AddressSubcommand::KeyHash {
                payment_verification_key_file,
            } => {
                let vk = load_verification_key(&payment_verification_key_file)?;
                let hash = vk.hash();
                println!("{}", hash.to_hex());
                Ok(())
            }
            AddressSubcommand::Build {
                payment_verification_key_file,
                stake_verification_key_file,
                network,
                out_file,
            } => {
                // Note: cardano-cli has no `--network` string flag (it takes
                // `--mainnet | --testnet-magic NATURAL`); this flag is a
                // dugite extension. An unknown value must be a hard error —
                // the old silent Testnet fallback turned typos like
                // "mainnnet" into valid-looking testnet addresses (#934).
                let network_id = match network.as_str() {
                    "mainnet" => NetworkId::Mainnet,
                    "testnet" | "testnet-magic" => NetworkId::Testnet,
                    other => anyhow::bail!(
                        "invalid --network value \"{other}\": accepted values are \
                         \"mainnet\" and \"testnet\" (synonym: \"testnet-magic\")"
                    ),
                };

                let payment_vk = load_verification_key(&payment_verification_key_file)?;
                let payment_hash = payment_vk.hash();
                let payment_cred = Credential::VerificationKey(payment_hash);

                let address = if let Some(stake_vk_file) = stake_verification_key_file {
                    let stake_vk = load_verification_key(&stake_vk_file)?;
                    let stake_hash = stake_vk.hash();
                    let stake_cred = Credential::VerificationKey(stake_hash);
                    Address::Base(BaseAddress {
                        network: network_id,
                        payment: payment_cred,
                        stake: stake_cred,
                    })
                } else {
                    Address::Enterprise(EnterpriseAddress {
                        network: network_id,
                        payment: payment_cred,
                    })
                };

                let addr_bytes = address.to_bytes();
                let hrp = match (&address, network_id) {
                    (
                        Address::Base(_) | Address::Enterprise(_) | Address::Pointer(_),
                        NetworkId::Mainnet,
                    ) => "addr",
                    (
                        Address::Base(_) | Address::Enterprise(_) | Address::Pointer(_),
                        NetworkId::Testnet,
                    ) => "addr_test",
                    _ => "addr",
                };

                let bech32_addr =
                    bech32::encode::<bech32::Bech32>(bech32::Hrp::parse(hrp)?, &addr_bytes)?;

                if let Some(out) = out_file {
                    std::fs::write(&out, &bech32_addr)?;
                    println!("Address written to: {}", out.display());
                } else {
                    println!("{}", bech32_addr);
                }

                Ok(())
            }
            AddressSubcommand::Info { address } => {
                println!("Address: {}", address);
                // Decode bech32 and show info
                let (hrp, data) = bech32::decode(&address)?;
                println!("HRP: {}", hrp);
                println!("Bytes: {} bytes", data.len());

                match Address::from_bytes(&data) {
                    Ok(addr) => match &addr {
                        Address::Base(a) => {
                            println!("Type: Base");
                            println!("Network: {:?}", a.network);
                            println!("Payment: {:?}", a.payment);
                            println!("Stake: {:?}", a.stake);
                        }
                        Address::Enterprise(a) => {
                            println!("Type: Enterprise");
                            println!("Network: {:?}", a.network);
                            println!("Payment: {:?}", a.payment);
                        }
                        Address::Reward(a) => {
                            println!("Type: Reward");
                            println!("Network: {:?}", a.network);
                            println!("Stake: {:?}", a.stake);
                        }
                        Address::Pointer(a) => {
                            println!("Type: Pointer");
                            println!("Network: {:?}", a.network);
                        }
                        Address::Byron(_) => {
                            println!("Type: Byron (legacy)");
                        }
                    },
                    Err(e) => {
                        println!("Could not decode address: {}", e);
                    }
                }

                Ok(())
            }
        }
    }
}

fn load_verification_key(path: &PathBuf) -> Result<PaymentVerificationKey> {
    let content = std::fs::read_to_string(path)?;
    let envelope: TextEnvelope = serde_json::from_str(&content)?;
    let cbor_bytes = hex::decode(&envelope.cbor_hex)?;
    let key_bytes = if cbor_bytes.len() > 2 {
        &cbor_bytes[2..]
    } else {
        &cbor_bytes
    };
    Ok(PaymentVerificationKey::from_bytes(key_bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── simple_cbor_wrap (this file's copy) ─────────────────────────────────

    /// The wrapper must produce *valid CBOR byte strings*: decode each output
    /// with minicbor and require the payload to round-trip. This guards the
    /// local copy against divergence from the one in node.rs.
    #[test]
    fn cbor_wrap_output_is_decodable_cbor_bytes() {
        for len in [0usize, 1, 23, 24, 32, 64, 255, 256, 612] {
            let payload = vec![0x5Au8; len];
            let wrapped = simple_cbor_wrap(&payload);
            let mut d = minicbor::Decoder::new(&wrapped);
            let decoded = d.bytes().unwrap_or_else(|e| {
                panic!("len={len}: wrap produced invalid CBOR: {e}");
            });
            assert_eq!(decoded, payload.as_slice(), "len={len}: payload mangled");
            assert_eq!(
                d.position(),
                wrapped.len(),
                "len={len}: trailing garbage after byte string"
            );
        }
    }

    // ── load_verification_key ────────────────────────────────────────────────

    /// Write a text envelope for the given raw cborHex string.
    fn write_envelope(dir: &std::path::Path, name: &str, cbor_hex: &str) -> PathBuf {
        let path = dir.join(name);
        let env = serde_json::json!({
            "type": "PaymentVerificationKeyShelley_ed25519",
            "description": "Payment Verification Key",
            "cborHex": cbor_hex
        });
        std::fs::write(&path, serde_json::to_string_pretty(&env).unwrap()).unwrap();
        path
    }

    #[test]
    fn load_verification_key_roundtrips_generated_key() {
        let dir = tempfile::tempdir().unwrap();
        let sk = dugite_crypto::keys::PaymentSigningKey::generate();
        let vk = sk.verification_key();
        let path = write_envelope(
            dir.path(),
            "gen.vkey",
            &hex::encode(simple_cbor_wrap(&vk.to_bytes())),
        );

        let loaded = load_verification_key(&path).unwrap();
        assert_eq!(loaded.to_bytes(), vk.to_bytes());
        assert_eq!(loaded.hash().as_bytes(), vk.hash().as_bytes());
    }

    #[test]
    fn load_verification_key_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.vkey");
        assert!(load_verification_key(&path).is_err());
    }

    #[test]
    fn load_verification_key_invalid_json_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("garbage.vkey");
        std::fs::write(&path, "{{{{").unwrap();
        assert!(load_verification_key(&path).is_err());
    }

    #[test]
    fn load_verification_key_bad_hex_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_envelope(dir.path(), "badhex.vkey", "zz-not-hex");
        assert!(load_verification_key(&path).is_err());
    }

    #[test]
    fn load_verification_key_wrong_length_errors() {
        let dir = tempfile::tempdir().unwrap();
        // CBOR bytes(16): too short for an Ed25519 verification key.
        let path = write_envelope(
            dir.path(),
            "short.vkey",
            &format!("5810{}", hex::encode([0u8; 16])),
        );
        assert!(load_verification_key(&path).is_err());
    }

    // ── AddressCmd::run flows via temp files ────────────────────────────────

    /// Generate a payment key envelope on disk, returning its path.
    fn generate_vkey_file(dir: &std::path::Path, name: &str) -> PathBuf {
        let sk = dugite_crypto::keys::PaymentSigningKey::generate();
        let vk = sk.verification_key();
        write_envelope(dir, name, &hex::encode(simple_cbor_wrap(&vk.to_bytes())))
    }

    #[test]
    fn build_enterprise_mainnet_address_shape() {
        let dir = tempfile::tempdir().unwrap();
        let vkey = generate_vkey_file(dir.path(), "payment.vkey");
        let out = dir.path().join("addr.txt");

        AddressCmd {
            command: AddressSubcommand::Build {
                payment_verification_key_file: vkey,
                stake_verification_key_file: None,
                network: "mainnet".to_string(),
                out_file: Some(out.clone()),
            },
        }
        .run()
        .unwrap();

        let addr = std::fs::read_to_string(&out).unwrap();
        assert!(
            addr.starts_with("addr1"),
            "mainnet address must use the `addr` HRP, got: {addr}"
        );
        let (hrp, bytes) = bech32::decode(&addr).unwrap();
        assert_eq!(hrp.as_str(), "addr");
        // Enterprise address: 1 header byte + 28-byte payment credential.
        assert_eq!(bytes.len(), 29);
        // Header: type 6 (enterprise/key), network 1 (mainnet) → 0x61.
        assert_eq!(bytes[0], 0x61);
    }

    #[test]
    fn build_base_testnet_address_shape() {
        let dir = tempfile::tempdir().unwrap();
        let payment = generate_vkey_file(dir.path(), "payment.vkey");
        let stake = generate_vkey_file(dir.path(), "stake.vkey");
        let out = dir.path().join("addr.txt");

        AddressCmd {
            command: AddressSubcommand::Build {
                payment_verification_key_file: payment,
                stake_verification_key_file: Some(stake),
                network: "testnet".to_string(),
                out_file: Some(out.clone()),
            },
        }
        .run()
        .unwrap();

        let addr = std::fs::read_to_string(&out).unwrap();
        assert!(
            addr.starts_with("addr_test1"),
            "testnet address must use the `addr_test` HRP, got: {addr}"
        );
        let (hrp, bytes) = bech32::decode(&addr).unwrap();
        assert_eq!(hrp.as_str(), "addr_test");
        // Base address: 1 header byte + 28 payment + 28 stake.
        assert_eq!(bytes.len(), 57);
        // Header: type 0 (base/key+key), network 0 (testnet) → 0x00.
        assert_eq!(bytes[0], 0x00);

        // The built address must parse back as a Base address.
        match Address::from_bytes(&bytes).unwrap() {
            Address::Base(a) => assert_eq!(a.network, NetworkId::Testnet),
            other => panic!("expected Base address, got {other:?}"),
        }
    }

    /// An unknown `--network` value must be a hard error naming the accepted
    /// forms — the old code silently fell back to Testnet, so a typo like
    /// "mainnnet" produced a testnet address without a word of warning.
    #[test]
    fn build_unknown_network_errors_listing_accepted_forms() {
        let dir = tempfile::tempdir().unwrap();
        let vkey = generate_vkey_file(dir.path(), "payment.vkey");

        let err = AddressCmd {
            command: AddressSubcommand::Build {
                payment_verification_key_file: vkey,
                stake_verification_key_file: None,
                network: "mainnnet".to_string(),
                out_file: None,
            },
        }
        .run()
        .expect_err("unknown --network value must be an error");
        let msg = err.to_string();
        assert!(msg.contains("mainnnet"), "must name the bad value: {msg}");
        assert!(
            msg.contains("mainnet") && msg.contains("testnet"),
            "must list accepted forms: {msg}"
        );
    }

    /// "testnet-magic" was historically accepted as a synonym for "testnet";
    /// keep it working.
    #[test]
    fn build_accepts_testnet_magic_network_synonym() {
        let dir = tempfile::tempdir().unwrap();
        let vkey = generate_vkey_file(dir.path(), "payment.vkey");
        let out = dir.path().join("addr.txt");

        AddressCmd {
            command: AddressSubcommand::Build {
                payment_verification_key_file: vkey,
                stake_verification_key_file: None,
                network: "testnet-magic".to_string(),
                out_file: Some(out.clone()),
            },
        }
        .run()
        .unwrap();
        let addr = std::fs::read_to_string(&out).unwrap();
        assert!(addr.starts_with("addr_test1"), "got: {addr}");
    }

    #[test]
    fn build_missing_payment_key_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = AddressCmd {
            command: AddressSubcommand::Build {
                payment_verification_key_file: dir.path().join("missing.vkey"),
                stake_verification_key_file: None,
                network: "mainnet".to_string(),
                out_file: None,
            },
        }
        .run();
        assert!(result.is_err(), "missing payment vkey must be an error");
    }

    #[test]
    fn info_accepts_built_address() {
        let dir = tempfile::tempdir().unwrap();
        let vkey = generate_vkey_file(dir.path(), "payment.vkey");
        let out = dir.path().join("addr.txt");
        AddressCmd {
            command: AddressSubcommand::Build {
                payment_verification_key_file: vkey,
                stake_verification_key_file: None,
                network: "testnet".to_string(),
                out_file: Some(out.clone()),
            },
        }
        .run()
        .unwrap();

        let addr = std::fs::read_to_string(&out).unwrap();
        AddressCmd {
            command: AddressSubcommand::Info { address: addr },
        }
        .run()
        .unwrap();
    }

    #[test]
    fn info_rejects_invalid_bech32() {
        let result = AddressCmd {
            command: AddressSubcommand::Info {
                address: "addr1qqinvalid!!checksum".to_string(),
            },
        }
        .run();
        assert!(result.is_err(), "invalid bech32 must be an error");
    }

    #[test]
    fn keygen_writes_loadable_key_pair() {
        let dir = tempfile::tempdir().unwrap();
        let vk_path = dir.path().join("kg.vkey");
        let sk_path = dir.path().join("kg.skey");

        AddressCmd {
            command: AddressSubcommand::KeyGen {
                verification_key_file: vk_path.clone(),
                signing_key_file: sk_path.clone(),
            },
        }
        .run()
        .unwrap();

        // The generated vkey file must load back through the same helper the
        // build command uses, and correspond to the generated skey.
        let vk = load_verification_key(&vk_path).unwrap();
        let sk_content = std::fs::read_to_string(&sk_path).unwrap();
        let sk_env: TextEnvelope = serde_json::from_str(&sk_content).unwrap();
        assert_eq!(sk_env.type_, "PaymentSigningKeyShelley_ed25519");
        let sk_cbor = hex::decode(&sk_env.cbor_hex).unwrap();
        let sk = dugite_crypto::keys::PaymentSigningKey::from_bytes(&sk_cbor[2..]).unwrap();
        assert_eq!(sk.verification_key().to_bytes(), vk.to_bytes());
    }
}
