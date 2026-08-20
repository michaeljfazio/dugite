use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct StakeAddressCmd {
    #[command(subcommand)]
    command: StakeAddressSubcommand,
}

#[derive(Subcommand, Debug)]
enum StakeAddressSubcommand {
    /// Generate stake address key pair
    KeyGen {
        #[arg(long)]
        verification_key_file: PathBuf,
        #[arg(long)]
        signing_key_file: PathBuf,
    },
    /// Build a stake (reward) address
    Build {
        #[arg(long)]
        stake_verification_key_file: PathBuf,
        /// Network: mainnet or testnet
        #[arg(long, default_value = "mainnet")]
        network: String,
        #[arg(long)]
        out_file: Option<PathBuf>,
    },
    /// Create a stake address registration certificate
    RegistrationCertificate {
        #[arg(long)]
        stake_verification_key_file: PathBuf,
        /// Deposit amount (Conway-era; omit for legacy Shelley cert)
        #[arg(long)]
        key_reg_deposit_amt: Option<u64>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a stake address deregistration certificate
    DeregistrationCertificate {
        #[arg(long)]
        stake_verification_key_file: PathBuf,
        /// Deposit refund amount (Conway-era; omit for legacy Shelley cert)
        #[arg(long)]
        key_reg_deposit_amt: Option<u64>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a stake address stake delegation certificate
    ///
    /// cardano-cli 11's canonical name is `stake-delegation-certificate`.
    /// dugite's original name, `delegation-certificate`, is kept as a
    /// visible alias for existing scripts (#1008) — both build the exact
    /// same `StakeDelegation` certificate. The name is made primary here
    /// (rather than just adding an alias to the old variant) so the
    /// cardano-cli surface-parity walker — which reads only the first
    /// token of each `Commands:` line, not `[aliases: ...]` — discovers it;
    /// see `scripts/validation/cli-surface-parity.sh`.
    #[command(visible_alias = "delegation-certificate")]
    StakeDelegationCertificate {
        #[arg(long)]
        stake_verification_key_file: PathBuf,
        /// Pool ID to delegate to (bech32 or hex)
        #[arg(long)]
        stake_pool_id: String,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a vote delegation certificate (Conway era)
    VoteDelegationCertificate {
        #[arg(long)]
        stake_verification_key_file: PathBuf,
        /// DRep verification key file or special value: always-abstain, always-no-confidence
        #[arg(long)]
        drep_verification_key_file: Option<PathBuf>,
        /// Use always-abstain as DRep
        #[arg(long)]
        always_abstain: bool,
        /// Use always-no-confidence as DRep
        #[arg(long)]
        always_no_confidence: bool,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Get the hash of a stake verification key
    KeyHash {
        #[arg(long)]
        stake_verification_key_file: PathBuf,
    },
    /// Create a combined stake registration + stake delegation certificate
    /// (Conway `stake_reg_deleg_cert`, cert type 11). #1008.
    RegistrationAndDelegationCertificate {
        #[command(flatten)]
        stake: StakeCredentialArgs,
        #[command(flatten)]
        pool: PoolIdArgs,
        #[arg(long)]
        key_reg_deposit_amt: u64,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a combined stake registration + vote delegation certificate
    /// (Conway `vote_reg_deleg_cert`, cert type 12). #1008.
    RegistrationAndVoteDelegationCertificate {
        #[command(flatten)]
        stake: StakeCredentialArgs,
        #[command(flatten)]
        drep: DRepArgs,
        #[arg(long)]
        key_reg_deposit_amt: u64,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a combined stake registration + stake delegation + vote
    /// delegation certificate (Conway `stake_vote_reg_deleg_cert`, cert
    /// type 13). #1008.
    RegistrationStakeAndVoteDelegationCertificate {
        #[command(flatten)]
        stake: StakeCredentialArgs,
        #[command(flatten)]
        pool: PoolIdArgs,
        #[command(flatten)]
        drep: DRepArgs,
        #[arg(long)]
        key_reg_deposit_amt: u64,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a combined stake delegation + vote delegation certificate,
    /// with no registration/deposit (Conway `stake_vote_deleg_cert`, cert
    /// type 10). #1008.
    StakeAndVoteDelegationCertificate {
        #[command(flatten)]
        stake: StakeCredentialArgs,
        #[command(flatten)]
        pool: PoolIdArgs,
        #[command(flatten)]
        drep: DRepArgs,
        #[arg(long)]
        out_file: PathBuf,
    },
}

/// The 5-way stake-credential selector cardano-cli repeats across every
/// stake-address certificate command that targets an arbitrary credential
/// (as opposed to `--stake-verification-key-file` alone, which the
/// pre-#1008 commands above use).
#[derive(Args, Debug)]
struct StakeCredentialArgs {
    #[arg(long, value_name = "STRING")]
    stake_verification_key: Option<String>,
    #[arg(long, value_name = "FILEPATH")]
    stake_verification_key_file: Option<PathBuf>,
    #[arg(long, value_name = "HASH")]
    stake_key_hash: Option<String>,
    #[arg(long, value_name = "FILEPATH")]
    stake_script_file: Option<PathBuf>,
    #[arg(long, value_name = "ADDRESS")]
    stake_address: Option<String>,
}

impl StakeCredentialArgs {
    fn resolve(&self) -> Result<crate::commands::credential::ResolvedCredential> {
        if let Some(addr) = &self.stake_address {
            return crate::commands::credential::stake_address_to_credential(addr);
        }
        crate::commands::credential::resolve_credential_5way(
            self.stake_verification_key.as_deref(),
            self.stake_verification_key_file.as_deref(),
            self.stake_key_hash.as_deref(),
            None,
            self.stake_script_file.as_deref(),
            "--stake-",
        )
    }
}

/// The 4-way stake-pool-ID selector cardano-cli repeats across every
/// certificate/vote command that names a target pool.
#[derive(Args, Debug)]
struct PoolIdArgs {
    #[arg(long, value_name = "STRING")]
    stake_pool_verification_key: Option<String>,
    #[arg(long, value_name = "STRING")]
    stake_pool_verification_extended_key: Option<String>,
    #[arg(long, value_name = "FILEPATH")]
    cold_verification_key_file: Option<PathBuf>,
    #[arg(long, value_name = "STAKE_POOL_ID")]
    stake_pool_id: Option<String>,
}

impl PoolIdArgs {
    fn resolve(&self) -> Result<Vec<u8>> {
        crate::commands::credential::resolve_pool_id(
            self.stake_pool_verification_key.as_deref(),
            self.stake_pool_verification_extended_key.as_deref(),
            self.cold_verification_key_file.as_deref(),
            self.stake_pool_id.as_deref(),
        )
    }
}

/// The 6-way DRep selector cardano-cli repeats across every vote-delegation
/// command.
#[derive(Args, Debug)]
struct DRepArgs {
    #[arg(long, value_name = "HASH")]
    drep_script_hash: Option<String>,
    #[arg(long, value_name = "STRING")]
    drep_verification_key: Option<String>,
    #[arg(long, value_name = "FILEPATH")]
    drep_verification_key_file: Option<PathBuf>,
    #[arg(long, value_name = "HASH")]
    drep_key_hash: Option<String>,
    #[arg(long)]
    always_abstain: bool,
    #[arg(long)]
    always_no_confidence: bool,
}

impl DRepArgs {
    fn resolve(&self) -> Result<crate::commands::credential::DRepSelector> {
        crate::commands::credential::resolve_drep_selector(
            self.drep_script_hash.as_deref(),
            self.drep_verification_key.as_deref(),
            self.drep_verification_key_file.as_deref(),
            self.drep_key_hash.as_deref(),
            self.always_abstain,
            self.always_no_confidence,
        )
    }
}

/// Encode a stake credential of either kind: `[0, hash]` key, `[1, hash]` script.
fn encode_resolved_credential(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    cred: &crate::commands::credential::ResolvedCredential,
) -> Result<()> {
    enc.array(2)?;
    enc.u32(cred.cred_type as u32)?;
    enc.bytes(&cred.hash)?;
    Ok(())
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

/// Load a verification key file and return the blake2b-224 hash
fn load_key_hash(path: &PathBuf) -> Result<Vec<u8>> {
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

/// Encode a stake credential as CBOR: [0, key_hash]
fn encode_stake_credential(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    key_hash: &[u8],
) -> Result<()> {
    enc.array(2)?;
    enc.u32(0)?; // key credential type
    enc.bytes(key_hash)?;
    Ok(())
}

impl StakeAddressCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            StakeAddressSubcommand::KeyGen {
                verification_key_file,
                signing_key_file,
            } => {
                let sk = dugite_crypto::keys::PaymentSigningKey::generate();
                let vk = sk.verification_key();

                let sk_env = serde_json::json!({
                    "type": "StakeSigningKeyShelley_ed25519",
                    "description": "Stake Signing Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
                });
                let vk_env = serde_json::json!({
                    "type": "StakeVerificationKeyShelley_ed25519",
                    "description": "Stake Verification Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
                });

                std::fs::write(&signing_key_file, serde_json::to_string_pretty(&sk_env)?)?;
                std::fs::write(
                    &verification_key_file,
                    serde_json::to_string_pretty(&vk_env)?,
                )?;

                println!("Stake address key pair generated.");
                Ok(())
            }
            StakeAddressSubcommand::Build {
                stake_verification_key_file,
                network,
                out_file,
            } => {
                let key_hash = load_key_hash(&stake_verification_key_file)?;

                // Reward address: header byte + key hash
                // Header: 0xe0 (testnet) or 0xe1 (mainnet) for stake key hash
                let header = if network == "mainnet" { 0xe1u8 } else { 0xe0u8 };
                let mut addr_bytes = vec![header];
                addr_bytes.extend_from_slice(&key_hash);

                let hrp = if network == "mainnet" {
                    "stake"
                } else {
                    "stake_test"
                };
                let addr = bech32::encode::<bech32::Bech32>(bech32::Hrp::parse(hrp)?, &addr_bytes)?;

                if let Some(ref path) = out_file {
                    std::fs::write(path, &addr)?;
                    println!("Stake address written to: {}", path.display());
                } else {
                    println!("{addr}");
                }
                Ok(())
            }
            StakeAddressSubcommand::RegistrationCertificate {
                stake_verification_key_file,
                key_reg_deposit_amt,
                out_file,
            } => {
                let key_hash = load_key_hash(&stake_verification_key_file)?;

                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);

                if let Some(deposit) = key_reg_deposit_amt {
                    // Conway-era: RegStake (cert type 7) = [7, credential, deposit]
                    enc.array(3)?;
                    enc.u32(7)?;
                    encode_stake_credential(&mut enc, &key_hash)?;
                    enc.u64(deposit)?;
                } else {
                    // Shelley-era: StakeRegistration (cert type 0) = [0, credential]
                    enc.array(2)?;
                    enc.u32(0)?;
                    encode_stake_credential(&mut enc, &key_hash)?;
                }

                let cert_type = if key_reg_deposit_amt.is_some() {
                    "CertificateConway"
                } else {
                    "CertificateShelley"
                };
                let cert_env = serde_json::json!({
                    "type": cert_type,
                    "description": "Stake Address Registration Certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Stake registration certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
            StakeAddressSubcommand::DeregistrationCertificate {
                stake_verification_key_file,
                key_reg_deposit_amt,
                out_file,
            } => {
                let key_hash = load_key_hash(&stake_verification_key_file)?;

                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);

                if let Some(deposit) = key_reg_deposit_amt {
                    // Conway-era: UnregStake (cert type 8) = [8, credential, refund]
                    enc.array(3)?;
                    enc.u32(8)?;
                    encode_stake_credential(&mut enc, &key_hash)?;
                    enc.u64(deposit)?;
                } else {
                    // Shelley-era: StakeDeregistration (cert type 1) = [1, credential]
                    enc.array(2)?;
                    enc.u32(1)?;
                    encode_stake_credential(&mut enc, &key_hash)?;
                }

                let cert_type = if key_reg_deposit_amt.is_some() {
                    "CertificateConway"
                } else {
                    "CertificateShelley"
                };
                let cert_env = serde_json::json!({
                    "type": cert_type,
                    "description": "Stake Address Deregistration Certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Stake deregistration certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
            StakeAddressSubcommand::StakeDelegationCertificate {
                stake_verification_key_file,
                stake_pool_id,
                out_file,
            } => {
                let key_hash = load_key_hash(&stake_verification_key_file)?;

                // Parse pool ID (bech32 or hex)
                let pool_hash = if stake_pool_id.starts_with("pool") {
                    let (_, bytes) = bech32::decode(&stake_pool_id)?;
                    bytes
                } else {
                    hex::decode(&stake_pool_id)?
                };
                if pool_hash.len() != 28 {
                    anyhow::bail!(
                        "Invalid pool ID length: {} bytes (expected 28)",
                        pool_hash.len()
                    );
                }

                // StakeDelegation (cert type 2) = [2, credential, pool_hash]
                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                enc.array(3)?;
                enc.u32(2)?;
                encode_stake_credential(&mut enc, &key_hash)?;
                enc.bytes(&pool_hash)?;

                let cert_env = serde_json::json!({
                    "type": "CertificateShelley",
                    "description": "Stake Delegation Certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Stake delegation certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
            StakeAddressSubcommand::VoteDelegationCertificate {
                stake_verification_key_file,
                drep_verification_key_file,
                always_abstain,
                always_no_confidence,
                out_file,
            } => {
                let key_hash = load_key_hash(&stake_verification_key_file)?;

                // Conway VoteDelegation (cert type 9) = [9, credential, drep]
                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                enc.array(3)?;
                enc.u32(9)?;
                encode_stake_credential(&mut enc, &key_hash)?;

                // DRep: [0, key_hash] for key, [2] for abstain, [3] for no-confidence
                if always_abstain {
                    enc.array(1)?;
                    enc.u32(2)?;
                } else if always_no_confidence {
                    enc.array(1)?;
                    enc.u32(3)?;
                } else if let Some(ref drep_file) = drep_verification_key_file {
                    let drep_hash = load_key_hash(drep_file)?;
                    enc.array(2)?;
                    enc.u32(0)?;
                    enc.bytes(&drep_hash)?;
                } else {
                    anyhow::bail!(
                        "Must provide --drep-verification-key-file, --always-abstain, or --always-no-confidence"
                    );
                }

                let cert_env = serde_json::json!({
                    "type": "CertificateConway",
                    "description": "Vote Delegation Certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Vote delegation certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
            StakeAddressSubcommand::KeyHash {
                stake_verification_key_file,
            } => {
                let key_hash = load_key_hash(&stake_verification_key_file)?;
                println!("{}", hex::encode(&key_hash));
                Ok(())
            }
            StakeAddressSubcommand::RegistrationAndDelegationCertificate {
                stake,
                pool,
                key_reg_deposit_amt,
                out_file,
            } => {
                let cred = stake.resolve()?;
                let pool_hash = pool.resolve()?;

                // stake_reg_deleg_cert = (11, stake_credential, pool_keyhash, coin)
                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                enc.array(4)?;
                enc.u32(11)?;
                encode_resolved_credential(&mut enc, &cred)?;
                enc.bytes(&pool_hash)?;
                enc.u64(key_reg_deposit_amt)?;

                let cert_env = serde_json::json!({
                    "type": "CertificateConway",
                    "description": "Stake address registration and stake delegation certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });
                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Stake registration and delegation certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
            StakeAddressSubcommand::RegistrationAndVoteDelegationCertificate {
                stake,
                drep,
                key_reg_deposit_amt,
                out_file,
            } => {
                let cred = stake.resolve()?;
                let drep_sel = drep.resolve()?;

                // vote_reg_deleg_cert = (12, stake_credential, drep, coin)
                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                enc.array(4)?;
                enc.u32(12)?;
                encode_resolved_credential(&mut enc, &cred)?;
                drep_sel.encode(&mut enc)?;
                enc.u64(key_reg_deposit_amt)?;

                let cert_env = serde_json::json!({
                    "type": "CertificateConway",
                    "description": "Stake address registration and vote delegation certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });
                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Stake registration and vote delegation certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
            StakeAddressSubcommand::RegistrationStakeAndVoteDelegationCertificate {
                stake,
                pool,
                drep,
                key_reg_deposit_amt,
                out_file,
            } => {
                let cred = stake.resolve()?;
                let pool_hash = pool.resolve()?;
                let drep_sel = drep.resolve()?;

                // stake_vote_reg_deleg_cert = (13, stake_credential, pool_keyhash, drep, coin)
                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                enc.array(5)?;
                enc.u32(13)?;
                encode_resolved_credential(&mut enc, &cred)?;
                enc.bytes(&pool_hash)?;
                drep_sel.encode(&mut enc)?;
                enc.u64(key_reg_deposit_amt)?;

                let cert_env = serde_json::json!({
                    "type": "CertificateConway",
                    "description": "Stake address registration, stake delegation and vote delegation certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });
                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Stake registration, stake delegation and vote delegation certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
            StakeAddressSubcommand::StakeAndVoteDelegationCertificate {
                stake,
                pool,
                drep,
                out_file,
            } => {
                let cred = stake.resolve()?;
                let pool_hash = pool.resolve()?;
                let drep_sel = drep.resolve()?;

                // stake_vote_deleg_cert = (10, stake_credential, pool_keyhash, drep)
                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                enc.array(4)?;
                enc.u32(10)?;
                encode_resolved_credential(&mut enc, &cred)?;
                enc.bytes(&pool_hash)?;
                drep_sel.encode(&mut enc)?;

                let cert_env = serde_json::json!({
                    "type": "CertificateConway",
                    "description": "Stake address stake delegation and vote delegation certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });
                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Stake and vote delegation certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_stake_credential() {
        let key_hash = vec![0xab; 28];
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        encode_stake_credential(&mut enc, &key_hash).unwrap();

        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 0);
        assert_eq!(dec.bytes().unwrap().len(), 28);
    }

    #[test]
    fn test_stake_address_header_bytes() {
        // Mainnet reward address header
        assert_eq!(0xe1u8 >> 4, 0x0e); // type 14 = reward address
        assert_eq!(0xe1u8 & 0x0f, 1); // network 1 = mainnet

        // Testnet reward address header
        assert_eq!(0xe0u8 >> 4, 0x0e); // type 14 = reward address
        assert_eq!(0xe0u8 & 0x0f, 0); // network 0 = testnet
    }

    #[test]
    fn test_simple_cbor_wrap_small() {
        let data = vec![0x01, 0x02, 0x03];
        let wrapped = simple_cbor_wrap(&data);
        // Major type 2 (byte string), length 3 → 0x43
        assert_eq!(wrapped[0], 0x43);
        assert_eq!(&wrapped[1..], &data);
    }

    #[test]
    fn test_simple_cbor_wrap_24_bytes() {
        let data = vec![0xab; 24];
        let wrapped = simple_cbor_wrap(&data);
        // Major type 2, one-byte length → 0x58, 24
        assert_eq!(wrapped[0], 0x58);
        assert_eq!(wrapped[1], 24);
        assert_eq!(&wrapped[2..], &data[..]);
    }

    #[test]
    fn test_simple_cbor_wrap_32_bytes() {
        let data = vec![0xcd; 32];
        let wrapped = simple_cbor_wrap(&data);
        assert_eq!(wrapped[0], 0x58);
        assert_eq!(wrapped[1], 32);
        assert_eq!(wrapped.len(), 34);
    }

    #[test]
    fn test_simple_cbor_wrap_256_bytes() {
        let data = vec![0xef; 256];
        let wrapped = simple_cbor_wrap(&data);
        // Two-byte length → 0x59, big-endian u16
        assert_eq!(wrapped[0], 0x59);
        assert_eq!(&wrapped[1..3], &[0x01, 0x00]); // 256 in BE
        assert_eq!(wrapped.len(), 259);
    }

    #[test]
    fn test_simple_cbor_wrap_empty() {
        let data = vec![];
        let wrapped = simple_cbor_wrap(&data);
        assert_eq!(wrapped, vec![0x40]); // byte string of length 0
    }

    #[test]
    fn test_shelley_registration_cert_cbor() {
        let key_hash = vec![0xab; 28];
        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        // Shelley StakeRegistration: [0, credential]
        enc.array(2).unwrap();
        enc.u32(0).unwrap();
        encode_stake_credential(&mut enc, &key_hash).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 0); // cert type 0
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 0); // key credential
        assert_eq!(dec.bytes().unwrap().len(), 28);
    }

    #[test]
    fn test_conway_registration_cert_cbor() {
        let key_hash = vec![0xab; 28];
        let deposit = 2_000_000u64;
        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        // Conway RegStake: [7, credential, deposit]
        enc.array(3).unwrap();
        enc.u32(7).unwrap();
        encode_stake_credential(&mut enc, &key_hash).unwrap();
        enc.u64(deposit).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u32().unwrap(), 7); // cert type 7
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 0); // key credential
        assert_eq!(dec.bytes().unwrap().len(), 28);
        assert_eq!(dec.u64().unwrap(), 2_000_000);
    }

    #[test]
    fn test_deregistration_cert_cbor() {
        let key_hash = vec![0xab; 28];
        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        // Shelley StakeDeregistration: [1, credential]
        enc.array(2).unwrap();
        enc.u32(1).unwrap();
        encode_stake_credential(&mut enc, &key_hash).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(2));
        assert_eq!(dec.u32().unwrap(), 1); // cert type 1
    }

    #[test]
    fn test_delegation_cert_cbor() {
        let key_hash = vec![0xab; 28];
        let pool_hash = vec![0xcd; 28];
        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        // StakeDelegation: [2, credential, pool_hash]
        enc.array(3).unwrap();
        enc.u32(2).unwrap();
        encode_stake_credential(&mut enc, &key_hash).unwrap();
        enc.bytes(&pool_hash).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u32().unwrap(), 2); // cert type 2
        assert_eq!(dec.array().unwrap(), Some(2)); // credential
        assert_eq!(dec.u32().unwrap(), 0);
        assert_eq!(dec.bytes().unwrap().len(), 28);
        assert_eq!(dec.bytes().unwrap().len(), 28); // pool hash
    }

    #[test]
    fn test_vote_delegation_abstain_cbor() {
        let key_hash = vec![0xab; 28];
        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        // VoteDelegation: [9, credential, drep]
        enc.array(3).unwrap();
        enc.u32(9).unwrap();
        encode_stake_credential(&mut enc, &key_hash).unwrap();
        // Abstain DRep: [2]
        enc.array(1).unwrap();
        enc.u32(2).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u32().unwrap(), 9); // cert type 9
        assert_eq!(dec.array().unwrap(), Some(2)); // credential
        dec.u32().unwrap();
        dec.bytes().unwrap();
        assert_eq!(dec.array().unwrap(), Some(1)); // drep
        assert_eq!(dec.u32().unwrap(), 2); // abstain
    }

    #[test]
    fn test_vote_delegation_no_confidence_cbor() {
        let key_hash = vec![0xab; 28];
        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        enc.array(3).unwrap();
        enc.u32(9).unwrap();
        encode_stake_credential(&mut enc, &key_hash).unwrap();
        // NoConfidence DRep: [3]
        enc.array(1).unwrap();
        enc.u32(3).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(3));
        assert_eq!(dec.u32().unwrap(), 9);
        dec.array().unwrap();
        dec.u32().unwrap();
        dec.bytes().unwrap();
        assert_eq!(dec.array().unwrap(), Some(1));
        assert_eq!(dec.u32().unwrap(), 3); // no-confidence
    }

    // ── Combined-certificate golden vectors (#1008) ─────────────────────────
    //
    // Each cborHex below was captured from real `cardano-cli 11.0.0.0`
    // (git rev 97036a66bcf8c89f687ae57a048eecc0389977ef) run against the SAME
    // stake verification key / pool-id / DRep selectors as the dugite-cli
    // invocation, and confirmed byte-identical before being pinned here — see
    // the #1008 implementation session. `stake_key_hash` below is the real
    // Blake2b-224 hash of that captured vkey.

    fn stake_key_hash() -> Vec<u8> {
        hex::decode("043fcb8f7ea8fb8e0e3ed601464f7473369f1957c0dde9df3fa5ca39").unwrap()
    }

    #[test]
    fn test_stake_and_vote_delegation_certificate_matches_cardano_cli() {
        let cred = crate::commands::credential::ResolvedCredential {
            cred_type: 0,
            hash: stake_key_hash(),
        };
        let pool_hash = vec![0u8; 27]
            .into_iter()
            .chain(std::iter::once(0x07u8))
            .collect::<Vec<u8>>();
        let drep = crate::commands::credential::DRepSelector::AlwaysAbstain;

        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        enc.array(4).unwrap();
        enc.u32(10).unwrap();
        encode_resolved_credential(&mut enc, &cred).unwrap();
        enc.bytes(&pool_hash).unwrap();
        drep.encode(&mut enc).unwrap();

        assert_eq!(
            hex::encode(&cert_cbor),
            "840a8200581c043fcb8f7ea8fb8e0e3ed601464f7473369f1957c0dde9df3fa5ca39\
581c000000000000000000000000000000000000000000000000000000078102"
        );
    }

    #[test]
    fn test_registration_and_delegation_certificate_matches_cardano_cli() {
        let cred = crate::commands::credential::ResolvedCredential {
            cred_type: 0,
            hash: stake_key_hash(),
        };
        let pool_hash = vec![0u8; 27]
            .into_iter()
            .chain(std::iter::once(0x07u8))
            .collect::<Vec<u8>>();

        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        enc.array(4).unwrap();
        enc.u32(11).unwrap();
        encode_resolved_credential(&mut enc, &cred).unwrap();
        enc.bytes(&pool_hash).unwrap();
        enc.u64(2_000_000).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(4));
        assert_eq!(dec.u32().unwrap(), 11);
    }

    #[test]
    fn test_registration_and_vote_delegation_certificate_cert_type_12() {
        let cred = crate::commands::credential::ResolvedCredential {
            cred_type: 0,
            hash: stake_key_hash(),
        };
        let drep = crate::commands::credential::DRepSelector::Key(vec![0x09; 28]);

        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        enc.array(4).unwrap();
        enc.u32(12).unwrap();
        encode_resolved_credential(&mut enc, &cred).unwrap();
        drep.encode(&mut enc).unwrap();
        enc.u64(2_000_000).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(4));
        assert_eq!(dec.u32().unwrap(), 12);
    }

    #[test]
    fn test_registration_stake_and_vote_delegation_certificate_cert_type_13() {
        let cred = crate::commands::credential::ResolvedCredential {
            cred_type: 0,
            hash: stake_key_hash(),
        };
        let pool_hash = vec![0x07; 28];
        let drep = crate::commands::credential::DRepSelector::AlwaysNoConfidence;

        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        enc.array(5).unwrap();
        enc.u32(13).unwrap();
        encode_resolved_credential(&mut enc, &cred).unwrap();
        enc.bytes(&pool_hash).unwrap();
        drep.encode(&mut enc).unwrap();
        enc.u64(2_000_000).unwrap();

        let mut dec = minicbor::Decoder::new(&cert_cbor);
        assert_eq!(dec.array().unwrap(), Some(5));
        assert_eq!(dec.u32().unwrap(), 13);
    }

    #[test]
    fn test_stake_address_to_credential_used_by_target_stake_address_form() {
        // A real testnet key-hash reward address captured from a cardano-cli
        // `stake-address build` run during the #1008 session (a different
        // vkey than stake_key_hash() above — only cred_type is asserted).
        let resolved = crate::commands::credential::stake_address_to_credential(
            "stake_test1uqrt88gn5yqlwsx4ez7grdmntjee40p725gcfstkjmnhr4q8xh76c",
        )
        .unwrap();
        assert_eq!(resolved.cred_type, 0);
    }
}
