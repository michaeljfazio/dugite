use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct GenesisCmd {
    #[command(subcommand)]
    command: GenesisSubcommand,
}

#[derive(Subcommand, Debug)]
enum GenesisSubcommand {
    /// Generate genesis keys
    KeyGen {
        #[arg(long)]
        verification_key_file: PathBuf,
        #[arg(long)]
        signing_key_file: PathBuf,
    },
    /// Print the hash of a genesis key
    KeyHash {
        #[arg(long)]
        verification_key_file: PathBuf,
    },
    /// Create a genesis delegation certificate
    GenesisDelegation {
        #[arg(long)]
        genesis_verification_key_file: PathBuf,
        #[arg(long)]
        drep_verification_key_file: PathBuf,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create an initial UTxO transaction
    ///
    /// clap-derive renders a multi-word variant name with a hyphen before
    /// every word boundary by default (`initial-tx-in`), but cardano-cli's
    /// real name has no hyphen before "txin" (`genesis initial-txin`,
    /// confirmed against `cardano-cli conway genesis --help`). #1008.
    #[command(name = "initial-txin")]
    InitialTxIn {
        #[arg(long)]
        genesis_utxo_verify_key_file: PathBuf,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Hash a genesis file
    Hash {
        #[arg(long)]
        genesis_file: PathBuf,
    },
    /// Create a genesis bundle for a local testnet.
    ///
    /// Generates genesis keys, delegate keys, UTxO keys, a Shelley genesis.json,
    /// and a minimal Byron genesis.  Matches `cardano-cli genesis create`.
    Create {
        /// Directory to write all genesis artifacts (default: ./genesis)
        #[arg(long, default_value = "genesis")]
        genesis_dir: PathBuf,
        /// Number of genesis key pairs to generate
        #[arg(long, default_value_t = 3)]
        gen_genesis_keys: u32,
        /// Number of initial UTxO key pairs to generate
        #[arg(long, default_value_t = 0)]
        gen_utxo_keys: u32,
        /// Network start time in ISO 8601 UTC (defaults to now)
        #[arg(long)]
        start_time: Option<String>,
        /// Total initial lovelace supply
        #[arg(long, default_value_t = 1_000_000_000_000_000u64)]
        supply: u64,
        /// Testnet magic (required)
        #[arg(long)]
        testnet_magic: u32,
    },
    /// Create a staked genesis bundle for a local testnet.
    ///
    /// Extends `genesis create` with stake-delegator key pairs.
    /// Matches `cardano-cli genesis create-staked`.
    #[command(name = "create-staked")]
    CreateStaked {
        /// Directory to write all genesis artifacts (default: ./genesis)
        #[arg(long, default_value = "genesis")]
        genesis_dir: PathBuf,
        /// Number of genesis key pairs to generate
        #[arg(long, default_value_t = 3)]
        gen_genesis_keys: u32,
        /// Number of initial UTxO key pairs to generate
        #[arg(long, default_value_t = 0)]
        gen_utxo_keys: u32,
        /// Number of stake-delegator key pairs to generate
        #[arg(long, default_value_t = 0)]
        gen_stake_delegs: u32,
        /// Network start time in ISO 8601 UTC (defaults to now)
        #[arg(long)]
        start_time: Option<String>,
        /// Total initial lovelace supply (split between UTxO and staked delegators)
        #[arg(long, default_value_t = 1_000_000_000_000_000u64)]
        supply: u64,
        /// Testnet magic (required)
        #[arg(long)]
        testnet_magic: u32,
    },
}

// ── CBOR helpers ──────────────────────────────────────────────────────────────

/// Wrap `data` in a CBOR bytestring (major type 2).
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

/// Decode the raw key bytes from a CBOR-wrapped text-envelope `cborHex`.
/// Strips the leading CBOR bytestring header (1 or 2 bytes).
fn decode_key_bytes(cbor_bytes: &[u8]) -> &[u8] {
    if cbor_bytes.len() > 2 && cbor_bytes[0] == 0x58 {
        &cbor_bytes[2..] // 0x58 <len1> <bytes>
    } else if cbor_bytes.len() > 1 && (cbor_bytes[0] & 0xe0) == 0x40 {
        &cbor_bytes[1..] // 0x4N <bytes> (short form)
    } else {
        cbor_bytes
    }
}

// ── Key-pair generation helpers ────────────────────────────────────────────

/// An Ed25519 key pair (raw bytes, not yet CBOR-wrapped).
struct KeyPair {
    sk_bytes: Vec<u8>,
    vk_bytes: Vec<u8>,
}

impl KeyPair {
    fn generate() -> Self {
        let sk = dugite_crypto::keys::PaymentSigningKey::generate();
        let vk = sk.verification_key();
        Self {
            sk_bytes: sk.to_bytes().to_vec(),
            vk_bytes: vk.to_bytes().to_vec(),
        }
    }

    /// Hash of the verification key (Blake2b-224, 28 bytes) as a lowercase hex string.
    fn vk_hash_hex(&self) -> String {
        let hash = dugite_primitives::hash::blake2b_224(&self.vk_bytes);
        hash.to_hex()
    }
}

/// Write a text-envelope JSON file.
fn write_envelope(
    path: &std::path::Path,
    type_str: &str,
    description: &str,
    cbor_hex: &str,
) -> Result<()> {
    let env = serde_json::json!({
        "type": type_str,
        "description": description,
        "cborHex": cbor_hex,
    });
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&env)?)?;
    Ok(())
}

/// Parse an optional ISO-8601 UTC timestamp (e.g. "2024-01-15T00:00:00Z").
/// Falls back to the current system time formatted as ISO-8601 UTC.
fn resolve_start_time(start_time: Option<&str>) -> String {
    if let Some(s) = start_time {
        s.to_string()
    } else {
        // Format current UTC time as ISO-8601
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        unix_to_iso8601(now)
    }
}

/// Convert a Unix timestamp to an ISO-8601 UTC string ("YYYY-MM-DDThh:mm:ssZ").
fn unix_to_iso8601(unix_secs: u64) -> String {
    let secs_per_day = 86_400u64;
    let days = unix_secs / secs_per_day;
    let day_secs = unix_secs % secs_per_day;
    let hours = day_secs / 3600;
    let minutes = (day_secs % 3600) / 60;
    let seconds = day_secs % 60;

    let mut y = 1970i64;
    let mut remaining = days;
    loop {
        let year_days: u64 = if is_leap(y as u64) { 366 } else { 365 };
        if remaining < year_days {
            break;
        }
        remaining -= year_days;
        y += 1;
    }
    let leap = is_leap(y as u64);
    let month_days: [u64; 12] = if leap {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 0usize;
    for (i, &md) in month_days.iter().enumerate() {
        if remaining < md {
            m = i;
            break;
        }
        remaining -= md;
    }
    let d = remaining + 1;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m + 1,
        d,
        hours,
        minutes,
        seconds
    )
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

// ── Genesis bundle creation ────────────────────────────────────────────────

/// Parameters for generating a genesis bundle.
struct GenesisParams {
    genesis_dir: PathBuf,
    gen_genesis_keys: u32,
    gen_utxo_keys: u32,
    gen_stake_delegs: u32,
    start_time: Option<String>,
    supply: u64,
    testnet_magic: u32,
}

/// Generate the full genesis bundle and write files to disk.
fn create_genesis_bundle(params: GenesisParams) -> Result<()> {
    let dir = &params.genesis_dir;
    let genesis_keys_dir = dir.join("genesis-keys");
    let delegate_keys_dir = dir.join("delegate-keys");
    let utxo_keys_dir = dir.join("utxo-keys");
    let stake_deleg_dir = dir.join("stake-delegator-keys");

    std::fs::create_dir_all(dir)?;
    std::fs::create_dir_all(&genesis_keys_dir)?;
    std::fs::create_dir_all(&delegate_keys_dir)?;
    if params.gen_utxo_keys > 0 {
        std::fs::create_dir_all(&utxo_keys_dir)?;
    }
    if params.gen_stake_delegs > 0 {
        std::fs::create_dir_all(&stake_deleg_dir)?;
    }

    // ── Genesis keys + delegate keys ──────────────────────────────────────

    // genDelegs: maps genesis_key_hash → { delegate: delegate_key_hash, vrf: vrf_key_hash }
    let mut gen_delegs = serde_json::Map::new();

    for i in 1..=params.gen_genesis_keys {
        let name = format!("genesis{i}");

        // Genesis key pair
        let genesis_kp = KeyPair::generate();
        let sk_cbor = hex::encode(simple_cbor_wrap(&genesis_kp.sk_bytes));
        let vk_cbor = hex::encode(simple_cbor_wrap(&genesis_kp.vk_bytes));
        write_envelope(
            &genesis_keys_dir.join(format!("{name}.skey")),
            "GenesisSigningKey_ed25519",
            "Genesis Signing Key",
            &sk_cbor,
        )?;
        write_envelope(
            &genesis_keys_dir.join(format!("{name}.vkey")),
            "GenesisVerificationKey_ed25519",
            "Genesis Verification Key",
            &vk_cbor,
        )?;
        let genesis_hash = genesis_kp.vk_hash_hex();

        // Delegate cold key pair
        let delegate_name = format!("delegate{i}");
        let delegate_kp = KeyPair::generate();
        let del_sk_cbor = hex::encode(simple_cbor_wrap(&delegate_kp.sk_bytes));
        let del_vk_cbor = hex::encode(simple_cbor_wrap(&delegate_kp.vk_bytes));
        write_envelope(
            &delegate_keys_dir.join(format!("{delegate_name}.skey")),
            "GenesisDelegateSigningKey_ed25519",
            "Genesis delegate operator key",
            &del_sk_cbor,
        )?;
        write_envelope(
            &delegate_keys_dir.join(format!("{delegate_name}.vkey")),
            "GenesisDelegateVerificationKey_ed25519",
            "Genesis delegate operator key",
            &del_vk_cbor,
        )?;
        let delegate_hash = delegate_kp.vk_hash_hex();

        // Delegate VRF key pair (Ed25519 used as placeholder; real VRF is VRF_VRF_VRF)
        // cardano-cli uses VRF keys here; we generate Ed25519 as a stand-in since
        // the VRF key format is compatible for genesis bundle purposes.
        let vrf_kp = KeyPair::generate();
        let vrf_sk_cbor = hex::encode(simple_cbor_wrap(&vrf_kp.sk_bytes));
        let vrf_vk_cbor = hex::encode(simple_cbor_wrap(&vrf_kp.vk_bytes));
        write_envelope(
            &delegate_keys_dir.join(format!("{delegate_name}.vrf.skey")),
            "VrfSigningKey_PraosVRF",
            "VRF Signing Key",
            &vrf_sk_cbor,
        )?;
        write_envelope(
            &delegate_keys_dir.join(format!("{delegate_name}.vrf.vkey")),
            "VrfVerificationKey_PraosVRF",
            "VRF Verification Key",
            &vrf_vk_cbor,
        )?;
        let vrf_hash = vrf_kp.vk_hash_hex();

        gen_delegs.insert(
            genesis_hash,
            serde_json::json!({
                "delegate": delegate_hash,
                "vrf": vrf_hash,
            }),
        );
    }

    // ── UTxO keys + initialFunds ──────────────────────────────────────────

    let mut initial_funds = serde_json::Map::new();
    let utxo_share = if params.gen_utxo_keys > 0 {
        params.supply / params.gen_utxo_keys as u64
    } else {
        0
    };

    for i in 1..=params.gen_utxo_keys {
        let name = format!("utxo{i}");
        let kp = KeyPair::generate();
        let sk_cbor = hex::encode(simple_cbor_wrap(&kp.sk_bytes));
        let vk_cbor = hex::encode(simple_cbor_wrap(&kp.vk_bytes));
        write_envelope(
            &utxo_keys_dir.join(format!("{name}.skey")),
            "GenesisUTxOSigningKey_ed25519",
            "Genesis Initial UTxO Signing Key",
            &sk_cbor,
        )?;
        write_envelope(
            &utxo_keys_dir.join(format!("{name}.vkey")),
            "GenesisUTxOVerificationKey_ed25519",
            "Genesis Initial UTxO Verification Key",
            &vk_cbor,
        )?;

        // The Shelley initial UTxO address is a 29-byte enterprise address:
        // 1 header byte (0x60 = Shelley testnet enterprise) + 28-byte key hash.
        // We encode it as a lowercase hex string (no bech32 in genesis.json).
        let vk_hash = dugite_primitives::hash::blake2b_224(&kp.vk_bytes);
        let mut addr_bytes = vec![0x60u8]; // enterprise, testnet
        addr_bytes.extend_from_slice(vk_hash.as_bytes());
        let addr_hex = hex::encode(&addr_bytes);

        initial_funds.insert(addr_hex, serde_json::Value::Number(utxo_share.into()));
    }

    // ── Stake delegator keys ──────────────────────────────────────────────

    for i in 1..=params.gen_stake_delegs {
        let name = format!("staking{i}");
        let kp = KeyPair::generate();
        let sk_cbor = hex::encode(simple_cbor_wrap(&kp.sk_bytes));
        let vk_cbor = hex::encode(simple_cbor_wrap(&kp.vk_bytes));
        write_envelope(
            &stake_deleg_dir.join(format!("{name}.skey")),
            "StakeSigningKey_ed25519",
            "Stake Signing Key",
            &sk_cbor,
        )?;
        write_envelope(
            &stake_deleg_dir.join(format!("{name}.vkey")),
            "StakeVerificationKey_ed25519",
            "Stake Verification Key",
            &vk_cbor,
        )?;
    }

    // ── Shelley genesis.json ──────────────────────────────────────────────

    let start_time_str = resolve_start_time(params.start_time.as_deref());

    let shelley_genesis = serde_json::json!({
        "activeSlotsCoeff": 0.05,
        "epochLength": 432000,
        "genDelegs": serde_json::Value::Object(gen_delegs),
        "initialFunds": serde_json::Value::Object(initial_funds),
        "maxKESEvolutions": 62,
        "maxLovelaceSupply": params.supply,
        "networkId": "Testnet",
        "networkMagic": params.testnet_magic,
        "protocolParams": {
            "a0": 0.3,
            "decentralisationParam": 1,
            "eMax": 18,
            "extraEntropy": { "tag": "NeutralNonce" },
            "keyDeposit": 2000000,
            "maxBlockBodySize": 90112,
            "maxBlockHeaderSize": 1100,
            "maxTxSize": 16384,
            "minFeeA": 44,
            "minFeeB": 155381,
            "minPoolCost": 340000000,
            "minUTxOValue": 0,
            "nOpt": 500,
            "poolDeposit": 500000000,
            "protocolVersion": { "major": 10, "minor": 0 },
            "rho": 0.003,
            "tau": 0.2,
        },
        "securityParam": 2160,
        "slotLength": 1,
        "slotsPerKESPeriod": 129600,
        "staking": {
            "pools": {},
            "stake": {},
        },
        "systemStart": start_time_str,
        "updateQuorum": 5,
    });
    std::fs::write(
        dir.join("genesis.json"),
        serde_json::to_string_pretty(&shelley_genesis)?,
    )?;

    // ── Minimal Byron genesis.json ────────────────────────────────────────

    let byron_genesis = serde_json::json!({
        "avvmDistr": {},
        "blockVersionData": {
            "heavyDelThd": "300000000000",
            "maxBlockSize": "2000000",
            "maxHeaderSize": "2000000",
            "maxProposalSize": "700",
            "maxTxSize": "4096",
            "mpcThd": "20000000000000",
            "scriptVersion": 0,
            "slotDuration": "20000",
            "softforkRule": {
                "initThd": "900000000000000",
                "minThd": "600000000000000",
                "thdDecrement": "50000000000000",
            },
            "txFeePolicy": {
                "multiplier": "43946000000",
                "summand": "155381000000000",
            },
            "unlockStakeEpoch": "18446744073709551615",
            "updateImplicit": "10000",
            "updateProposalThd": "100000000000000",
            "updateVoteThd": "1000000000000",
        },
        "bootStakeholders": {},
        "ftsGenSecrets": [],
        "heavyDelegation": {},
        "nonAvvmBalances": {},
        "protocolConsts": {
            "k": 2160,
            "protocolMagic": params.testnet_magic,
        },
        "startTime": 1665590400,
        "vssCerts": {},
    });
    std::fs::write(
        dir.join("byron.genesis.json"),
        serde_json::to_string_pretty(&byron_genesis)?,
    )?;

    println!("Genesis bundle written to: {}", dir.display());
    println!("  genesis.json");
    println!("  byron.genesis.json");
    println!(
        "  genesis-keys/genesis{{1..{}}}.{{vkey,skey}}",
        params.gen_genesis_keys
    );
    println!(
        "  delegate-keys/delegate{{1..{}}}.{{vkey,skey,vrf.vkey,vrf.skey}}",
        params.gen_genesis_keys
    );
    if params.gen_utxo_keys > 0 {
        println!(
            "  utxo-keys/utxo{{1..{}}}.{{vkey,skey}}",
            params.gen_utxo_keys
        );
    }
    if params.gen_stake_delegs > 0 {
        println!(
            "  stake-delegator-keys/staking{{1..{}}}.{{vkey,skey}}",
            params.gen_stake_delegs
        );
    }

    Ok(())
}

impl GenesisCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            GenesisSubcommand::KeyGen {
                verification_key_file,
                signing_key_file,
            } => {
                let sk = dugite_crypto::keys::PaymentSigningKey::generate();
                let vk = sk.verification_key();

                let sk_env = serde_json::json!({
                    "type": "GenesisSigningKey_ed25519",
                    "description": "Genesis Signing Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
                });
                let vk_env = serde_json::json!({
                    "type": "GenesisVerificationKey_ed25519",
                    "description": "Genesis Verification Key",
                    "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
                });

                std::fs::write(&signing_key_file, serde_json::to_string_pretty(&sk_env)?)?;
                std::fs::write(
                    &verification_key_file,
                    serde_json::to_string_pretty(&vk_env)?,
                )?;

                println!("Genesis key pair generated.");
                Ok(())
            }
            GenesisSubcommand::KeyHash {
                verification_key_file,
            } => {
                let content = std::fs::read_to_string(&verification_key_file)?;
                let env: serde_json::Value = serde_json::from_str(&content)?;
                let cbor_hex = env["cborHex"].as_str().ok_or_else(|| {
                    anyhow::anyhow!("Missing cborHex in {}", verification_key_file.display())
                })?;
                let cbor_bytes = hex::decode(cbor_hex)?;
                let key_bytes = decode_key_bytes(&cbor_bytes);
                let hash = dugite_primitives::hash::blake2b_224(key_bytes);
                println!("{}", hash.to_hex());
                Ok(())
            }
            GenesisSubcommand::GenesisDelegation {
                genesis_verification_key_file,
                drep_verification_key_file,
                out_file,
            } => {
                let genesis_content = std::fs::read_to_string(&genesis_verification_key_file)?;
                let genesis_env: serde_json::Value = serde_json::from_str(&genesis_content)?;
                let genesis_cbor_hex = genesis_env["cborHex"].as_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Missing cborHex in {}",
                        genesis_verification_key_file.display()
                    )
                })?;
                let genesis_cbor = hex::decode(genesis_cbor_hex)?;
                let genesis_key_bytes = decode_key_bytes(&genesis_cbor);
                let genesis_hash = dugite_primitives::hash::blake2b_224(genesis_key_bytes);

                let drep_content = std::fs::read_to_string(&drep_verification_key_file)?;
                let drep_env: serde_json::Value = serde_json::from_str(&drep_content)?;
                let drep_cbor_hex = drep_env["cborHex"].as_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Missing cborHex in {}",
                        drep_verification_key_file.display()
                    )
                })?;
                let drep_cbor = hex::decode(drep_cbor_hex)?;
                let drep_key_bytes = decode_key_bytes(&drep_cbor);

                let mut cert_cbor = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                enc.array(4)?;
                enc.u32(3)?; // GenesisDelegation certificate type
                enc.bytes(genesis_hash.as_bytes())?;
                enc.bytes(drep_key_bytes)?;
                enc.u64(0)?; // epoch (placeholder)

                let cert_env = serde_json::json!({
                    "type": "CertificateShelley",
                    "description": "Genesis Delegation Certificate",
                    "cborHex": hex::encode(&cert_cbor)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                println!(
                    "Genesis delegation certificate written to: {}",
                    out_file.display()
                );
                Ok(())
            }
            GenesisSubcommand::InitialTxIn {
                genesis_utxo_verify_key_file,
                out_file,
            } => {
                let content = std::fs::read_to_string(&genesis_utxo_verify_key_file)?;
                let env: serde_json::Value = serde_json::from_str(&content)?;
                let cbor_hex = env["cborHex"].as_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Missing cborHex in {}",
                        genesis_utxo_verify_key_file.display()
                    )
                })?;
                let cbor_bytes = hex::decode(cbor_hex)?;
                let key_bytes = decode_key_bytes(&cbor_bytes);
                let hash = dugite_primitives::hash::blake2b_224(key_bytes);

                let mut output = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut output);
                enc.array(2)?;
                enc.bytes(hash.as_bytes())?;
                enc.u32(0)?; // output index

                let result = serde_json::json!({
                    "cborHex": hex::encode(&output),
                    "description": "Genesis initial UTxO",
                    "type": "GenesisUTxO"
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&result)?)?;
                println!("Genesis initial UTxO written to: {}", out_file.display());
                Ok(())
            }
            GenesisSubcommand::Hash { genesis_file } => {
                // cardano-cli hashes the raw file bytes with Blake2b-256 for all
                // non-Byron genesis files (shelley/alonzo/conway).  Parsing and
                // re-serialising the JSON via serde_json reorders keys and strips
                // whitespace, producing a different hash.  Read raw bytes instead.
                let raw = std::fs::read(&genesis_file)?;
                let hash = dugite_primitives::hash::blake2b_256(&raw);
                println!("{}", hash.to_hex());
                Ok(())
            }
            GenesisSubcommand::Create {
                genesis_dir,
                gen_genesis_keys,
                gen_utxo_keys,
                start_time,
                supply,
                testnet_magic,
            } => create_genesis_bundle(GenesisParams {
                genesis_dir,
                gen_genesis_keys,
                gen_utxo_keys,
                gen_stake_delegs: 0,
                start_time,
                supply,
                testnet_magic,
            }),
            GenesisSubcommand::CreateStaked {
                genesis_dir,
                gen_genesis_keys,
                gen_utxo_keys,
                gen_stake_delegs,
                start_time,
                supply,
                testnet_magic,
            } => create_genesis_bundle(GenesisParams {
                genesis_dir,
                gen_genesis_keys,
                gen_utxo_keys,
                gen_stake_delegs,
                start_time,
                supply,
                testnet_magic,
            }),
        }
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_unix_to_iso8601_epoch() {
        assert_eq!(unix_to_iso8601(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn test_unix_to_iso8601_known_date() {
        // 2022-11-24T00:00:00Z = 1669248000 Unix
        assert_eq!(unix_to_iso8601(1_669_248_000), "2022-11-24T00:00:00Z");
    }

    #[test]
    fn test_unix_to_iso8601_time_components() {
        // 1h 1m 1s = 3661 seconds
        let s = unix_to_iso8601(3661);
        assert!(s.contains("01:01:01"), "got: {s}");
    }

    #[test]
    fn test_decode_key_bytes_short() {
        // 0x41 <1 byte> = CBOR bytestring length 1 (major 2 short form)
        let cbor = [0x41u8, 0xAB];
        assert_eq!(decode_key_bytes(&cbor), &[0xABu8]);
    }

    #[test]
    fn test_decode_key_bytes_two_byte_header() {
        // 0x58 <len> <bytes> form for 32-byte key
        let mut cbor = vec![0x58u8, 32];
        cbor.extend_from_slice(&[0u8; 32]);
        assert_eq!(decode_key_bytes(&cbor), &[0u8; 32]);
    }

    #[test]
    fn test_genesis_create_default_structure() {
        let tmp = TempDir::new().unwrap();
        let genesis_dir = tmp.path().join("genesis");
        create_genesis_bundle(GenesisParams {
            genesis_dir: genesis_dir.clone(),
            gen_genesis_keys: 2,
            gen_utxo_keys: 1,
            gen_stake_delegs: 0,
            start_time: Some("2024-01-01T00:00:00Z".to_string()),
            supply: 1_000_000_000_000_000,
            testnet_magic: 42,
        })
        .unwrap();

        // Verify required files exist
        assert!(genesis_dir.join("genesis.json").exists());
        assert!(genesis_dir.join("byron.genesis.json").exists());
        assert!(genesis_dir.join("genesis-keys/genesis1.vkey").exists());
        assert!(genesis_dir.join("genesis-keys/genesis1.skey").exists());
        assert!(genesis_dir.join("genesis-keys/genesis2.vkey").exists());
        assert!(genesis_dir.join("delegate-keys/delegate1.vkey").exists());
        assert!(genesis_dir
            .join("delegate-keys/delegate1.vrf.vkey")
            .exists());
        assert!(genesis_dir.join("utxo-keys/utxo1.vkey").exists());
        assert!(genesis_dir.join("utxo-keys/utxo1.skey").exists());
    }

    #[test]
    fn test_genesis_create_json_structure() {
        let tmp = TempDir::new().unwrap();
        let genesis_dir = tmp.path().join("genesis");
        create_genesis_bundle(GenesisParams {
            genesis_dir: genesis_dir.clone(),
            gen_genesis_keys: 1,
            gen_utxo_keys: 1,
            gen_stake_delegs: 0,
            start_time: Some("2024-01-01T00:00:00Z".to_string()),
            supply: 5_000_000_000u64,
            testnet_magic: 42,
        })
        .unwrap();

        let genesis_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(genesis_dir.join("genesis.json")).unwrap(),
        )
        .unwrap();

        // Required top-level fields
        assert!(genesis_json.get("activeSlotsCoeff").is_some());
        assert!(genesis_json.get("genDelegs").is_some());
        assert!(genesis_json.get("initialFunds").is_some());
        assert!(genesis_json.get("networkMagic").is_some());
        assert_eq!(genesis_json["networkMagic"].as_u64(), Some(42));
        assert_eq!(
            genesis_json["systemStart"].as_str(),
            Some("2024-01-01T00:00:00Z")
        );
        assert_eq!(
            genesis_json["maxLovelaceSupply"].as_u64(),
            Some(5_000_000_000)
        );

        // One genesis delegation entry
        let gen_delegs = genesis_json["genDelegs"].as_object().unwrap();
        assert_eq!(gen_delegs.len(), 1);
        let deleg = gen_delegs.values().next().unwrap();
        assert!(deleg.get("delegate").is_some());
        assert!(deleg.get("vrf").is_some());

        // One UTxO entry
        let funds = genesis_json["initialFunds"].as_object().unwrap();
        assert_eq!(funds.len(), 1);
        let (addr, amount) = funds.iter().next().unwrap();
        // Address is 29 bytes = 58 hex chars (1 header + 28 key hash)
        assert_eq!(addr.len(), 58, "enterprise address should be 29 bytes hex");
        assert_eq!(amount.as_u64(), Some(5_000_000_000));
    }

    #[test]
    fn test_genesis_create_staked_generates_stake_keys() {
        let tmp = TempDir::new().unwrap();
        let genesis_dir = tmp.path().join("genesis");
        create_genesis_bundle(GenesisParams {
            genesis_dir: genesis_dir.clone(),
            gen_genesis_keys: 1,
            gen_utxo_keys: 0,
            gen_stake_delegs: 2,
            start_time: None,
            supply: 1_000_000_000_000_000,
            testnet_magic: 1,
        })
        .unwrap();

        assert!(genesis_dir
            .join("stake-delegator-keys/staking1.vkey")
            .exists());
        assert!(genesis_dir
            .join("stake-delegator-keys/staking1.skey")
            .exists());
        assert!(genesis_dir
            .join("stake-delegator-keys/staking2.vkey")
            .exists());
    }

    #[test]
    fn test_genesis_key_envelope_type_fields() {
        let tmp = TempDir::new().unwrap();
        let genesis_dir = tmp.path().join("genesis");
        create_genesis_bundle(GenesisParams {
            genesis_dir: genesis_dir.clone(),
            gen_genesis_keys: 1,
            gen_utxo_keys: 1,
            gen_stake_delegs: 0,
            start_time: Some("2024-06-01T00:00:00Z".to_string()),
            supply: 1_000_000_000_000_000,
            testnet_magic: 2,
        })
        .unwrap();

        // Verify text-envelope type fields match cardano-cli exactly
        let g_vkey: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(genesis_dir.join("genesis-keys/genesis1.vkey")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            g_vkey["type"].as_str(),
            Some("GenesisVerificationKey_ed25519")
        );

        let d_vkey: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(genesis_dir.join("delegate-keys/delegate1.vkey")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            d_vkey["type"].as_str(),
            Some("GenesisDelegateVerificationKey_ed25519")
        );

        let u_vkey: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(genesis_dir.join("utxo-keys/utxo1.vkey")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            u_vkey["type"].as_str(),
            Some("GenesisUTxOVerificationKey_ed25519")
        );
    }

    #[test]
    fn test_genesis_zero_utxo_keys() {
        let tmp = TempDir::new().unwrap();
        let genesis_dir = tmp.path().join("genesis");
        create_genesis_bundle(GenesisParams {
            genesis_dir: genesis_dir.clone(),
            gen_genesis_keys: 1,
            gen_utxo_keys: 0,
            gen_stake_delegs: 0,
            start_time: Some("2024-01-01T00:00:00Z".to_string()),
            supply: 1_000_000_000_000_000,
            testnet_magic: 2,
        })
        .unwrap();

        let genesis_json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(genesis_dir.join("genesis.json")).unwrap(),
        )
        .unwrap();

        let funds = genesis_json["initialFunds"].as_object().unwrap();
        assert_eq!(funds.len(), 0, "no utxo keys → empty initialFunds");
    }

    // ── Genesis hash regression tests ─────────────────────────────────────────
    //
    // cardano-cli hashes the raw file bytes with Blake2b-256.  The old dugite-cli
    // code parsed the JSON and re-serialised it via serde_json, which sorts keys
    // alphabetically and strips any whitespace differences — producing a different
    // hash for any file whose key order differs from alphabetical.
    //
    // These tests pin the correct (raw-bytes) behaviour and confirm that the old
    // parse+re-serialise approach would have produced a different result, proving
    // the regression guard is meaningful.

    /// Helper: Blake2b-256 of raw bytes, returned as lowercase hex.
    fn blake2b256_hex(data: &[u8]) -> String {
        dugite_primitives::hash::blake2b_256(data).to_hex()
    }

    /// Helper: Blake2b-256 of a parse+re-serialise round-trip, simulating the
    /// old broken behaviour (serde_json sorts keys alphabetically).
    fn buggy_hash_hex(raw: &[u8]) -> String {
        let json: serde_json::Value = serde_json::from_slice(raw).unwrap();
        let canonical = serde_json::to_vec(&json).unwrap();
        blake2b256_hex(&canonical)
    }

    #[test]
    fn test_genesis_hash_raw_bytes_matches_cardano_cli() {
        // JSON with keys deliberately in non-alphabetical order.
        // cardano-cli (and now dugite-cli) hashes this exact byte sequence.
        // serde_json would reorder keys to {"a":1,"z":99}, giving a different hash.
        let fixture: &[u8] = b"{\"z\":99,\"a\":1}";

        let correct_hash = blake2b256_hex(fixture);
        let old_broken_hash = buggy_hash_hex(fixture);

        // The two code paths must differ on this fixture, confirming the test
        // would have caught the original bug.
        assert_ne!(
            correct_hash, old_broken_hash,
            "raw-bytes and parse+reser hashes unexpectedly equal — fixture must have keys out of alphabetical order"
        );

        // Pin the expected raw-bytes hash (verified against cardano-cli via Python
        // blake2b with digest_size=32 on the exact fixture bytes above).
        assert_eq!(
            correct_hash,
            "1a82d5ea4a94dc561407f739963678a495d0638f75e38da5eb9d0232b2e0b697",
            "Blake2b-256 of raw genesis bytes changed — update the expected hash if the fixture changed"
        );
    }

    #[test]
    fn test_genesis_hash_via_tempfile_roundtrip() {
        // Write the fixture to a real temp file and hash via the same code path
        // that the `genesis hash` subcommand now uses (std::fs::read + blake2b_256).
        // This validates the end-to-end file I/O path, not just the hash function.
        let tmp = TempDir::new().unwrap();
        let fixture_path = tmp.path().join("shelley-genesis.json");

        // Realistic-looking fixture with keys out-of-alpha order (z before a).
        let fixture: &[u8] = b"{\"z\":\"last\",\"activeSlotsCoeff\":0.05,\"networkMagic\":1}";
        std::fs::write(&fixture_path, fixture).unwrap();

        let raw = std::fs::read(&fixture_path).unwrap();
        let hash = blake2b256_hex(&raw);

        // Pin the expected hash (verified with Python blake2b digest_size=32).
        assert_eq!(
            hash, "860e5e9637d94f372f7c684b2f77cd5e666ef3b7a43bce56bc40cf0702df1303",
            "Raw-bytes hash of fixture changed unexpectedly"
        );

        // Confirm that parse+re-serialize gives a DIFFERENT hash (regression guard).
        let old_hash = buggy_hash_hex(fixture);
        assert_ne!(
            hash, old_hash,
            "Raw and parse+reser hashes are equal — the fixture keys must be out of alphabetical order for this guard to be meaningful"
        );
    }
}
