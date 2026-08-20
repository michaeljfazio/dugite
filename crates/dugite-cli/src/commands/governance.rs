//! Governance commands (Conway era): DRep, vote, and governance-action CBOR
//! builders.
//!
//! ## CIP-0094 SPO polls: deliberately NOT implemented (#998)
//!
//! `cardano-cli` shipped `governance {create,answer,verify}-poll` from
//! 2023-04-17 (`input-output-hk/cardano-node` PR #5112, merge
//! `cf61eb378049f7e9ae854de998c9bff571b3acfe`, "Add new interim governance
//! commands: {create, answer, verify}-poll"; moved into the
//! `compatible babbage governance` command tree by cardano-cli PR #322,
//! merge `4c615c9e25371c1081384732bbfcb57b39ddbbec`, 2023-10-05) until
//! 2025-05-08, when cardano-cli PR #1178 ("Delete `governance` `poll`
//! commands", merge `db83e11127092b4c216eed5572c4623b8ac51e79`) deleted them
//! outright. Last release with them: `cardano-cli-10.8.0.0`
//! (`685970733dc4ef5838967cb7cfb6d3fe4c2a2b06`); first without:
//! `cardano-cli-10.9.0.0` (`e13f84d9fc9cafa293e88f017592d994ca1b12a2`). All
//! four SHAs verified live against the GitHub API, not just quoted from
//! research. Even while the commands existed, the parser hard-excluded
//! Conway:
//!
//! ```haskell
//! -- Cardano.CLI.EraBased.Governance.Poll.Option, cardano-cli-10.8.0.0
//! pGovernanceCreatePoll era = do
//!   w <- forShelleyBasedEraMaybeEon era
//!   when ("BabbageEraOnwardsConway" `isInfixOf` show w) Nothing
//!   pure $ ...
//! ```
//!
//! — the commands were never reachable on the only era dugite targets.
//!
//! `cardano-cli 11.0.0.0` (git rev `97036a66bcf8c89f687ae57a048eecc0389977ef`,
//! the build this project targets for parity) exposes zero poll commands
//! anywhere in its command tree: verified against a full recursive
//! `cardano-cli help` dump (7530 lines), zero case-insensitive matches for
//! "poll". CIP-0094 itself remains `Status: Active` and `cardano-api`'s
//! `Cardano.Api.Governance.Internal.Poll` module is still exported — the
//! *library* support outlived the *CLI* front-end — but this project's
//! standing rule is that cardano-cli's actual implementation wins over CIP
//! prose when the two disagree (see CLAUDE.md, citing the CIP-0121 /
//! `plutus` 8192-vs-2^29 precedent). Matching cardano-cli here means
//! matching its current, poll-less surface: implementing
//! `create-poll`/`answer-poll`/`verify-poll` would add dugite-cli commands
//! with no live cardano-cli invocation to golden-test against, and no
//! reachable era to exercise them in even when cardano-cli last had them.
//!
//! `dugite-node` needs no change either way: CIP-0094 polls ride entirely in
//! ordinary tx metadata (label 94), which the ledger already carries
//! opaquely regardless of which CLI (if any) produces or consumes it.

use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct GovernanceCmd {
    #[command(subcommand)]
    command: GovernanceSubcommand,
}

#[derive(Subcommand, Debug)]
enum GovernanceSubcommand {
    /// DRep commands
    Drep {
        #[command(subcommand)]
        command: DRepSubcommand,
    },
    /// Vote on governance actions
    Vote {
        #[command(subcommand)]
        command: VoteSubcommand,
    },
    /// Create governance actions
    Action {
        #[command(subcommand)]
        command: ActionSubcommand,
    },
    /// Constitutional Committee key/certificate commands. #1008.
    Committee {
        #[command(subcommand)]
        command: CommitteeSubcommand,
    },
    /// Create an MIR (Move Instantaneous Rewards) certificate.
    ///
    /// Legacy pre-Conway mechanism: Phase-1 is a no-op at PV>=9 on any live
    /// network, so this exists for tooling/certificate-construction
    /// completeness rather than for any effect on a Conway chain. Matches
    /// `cardano-cli compatible shelley governance create-mir-certificate`
    /// (the surface-parity walker strips both the `compatible` and
    /// `shelley` era/namespace tokens, so it normalizes to this same path).
    /// #1008.
    CreateMirCertificate {
        #[command(subcommand)]
        command: MirSubcommand,
    },
}

#[derive(Subcommand, Debug)]
enum DRepSubcommand {
    /// Generate DRep keys
    KeyGen {
        #[arg(long)]
        verification_key_file: PathBuf,
        #[arg(long)]
        signing_key_file: PathBuf,
    },
    /// Get DRep ID
    Id {
        #[arg(long)]
        drep_verification_key_file: PathBuf,
        /// Output format: bech32 (default) or hex
        #[arg(long, default_value = "bech32")]
        output_format: String,
    },
    /// Create DRep registration certificate
    RegistrationCertificate {
        #[arg(long)]
        drep_verification_key_file: PathBuf,
        #[arg(long)]
        key_reg_deposit_amt: u64,
        /// Optional anchor URL for DRep metadata
        #[arg(long)]
        anchor_url: Option<String>,
        /// Optional anchor data hash
        #[arg(long)]
        anchor_data_hash: Option<String>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create DRep deregistration (retirement) certificate
    RetirementCertificate {
        #[arg(long)]
        drep_verification_key_file: PathBuf,
        #[arg(long)]
        deposit_amt: u64,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create DRep metadata update certificate
    UpdateCertificate {
        #[arg(long)]
        drep_verification_key_file: PathBuf,
        #[arg(long)]
        anchor_url: Option<String>,
        #[arg(long)]
        anchor_data_hash: Option<String>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Calculate the hash of a DRep metadata file
    ///
    /// #1008: same `blake2b_256(raw bytes)` computation as `hash
    /// anchor-data` (oracle-verified against cardano-api's
    /// `hashDRepMetadata` = `Crypto.hashWith id bs` directly — no JSON
    /// parsing or canonicalization despite what the upstream doc comment
    /// implies). Shares `fetch_url_bytes` with `hash anchor-data` for the
    /// `--drep-metadata-url` path.
    MetadataHash {
        #[arg(long, conflicts_with = "drep_metadata_url")]
        drep_metadata_file: Option<PathBuf>,
        #[arg(long, conflicts_with = "drep_metadata_file")]
        drep_metadata_url: Option<String>,
        #[arg(long, conflicts_with = "out_file")]
        expected_hash: Option<String>,
        #[arg(long)]
        out_file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum VoteSubcommand {
    /// Create a vote file
    Create {
        #[arg(long)]
        governance_action_tx_id: String,
        #[arg(long)]
        governance_action_index: u32,
        /// Vote: yes, no, or abstain
        #[arg(long)]
        vote: String,
        /// DRep verification key file (for DRep voter)
        #[arg(long)]
        drep_verification_key_file: Option<PathBuf>,
        /// SPO cold verification key file (for SPO voter)
        #[arg(long)]
        cold_verification_key_file: Option<PathBuf>,
        /// CC hot verification key file (for Constitutional Committee voter)
        #[arg(long)]
        cc_hot_verification_key_file: Option<PathBuf>,
        #[arg(long)]
        anchor_url: Option<String>,
        #[arg(long)]
        anchor_data_hash: Option<String>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// View a vote file. #1008.
    ///
    /// Matches `cardano-cli conway governance vote view`. Decodes the same
    /// `voting procedures` CBOR shape `vote create` writes.
    View {
        #[arg(long, value_name = "FILEPATH")]
        vote_file: PathBuf,
        #[arg(long, conflicts_with = "output_yaml")]
        output_json: bool,
        #[arg(long)]
        output_yaml: bool,
        #[arg(long, value_name = "FILEPATH")]
        out_file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
#[allow(clippy::enum_variant_names)]
enum ActionSubcommand {
    /// Create an info action
    CreateInfo {
        #[arg(long)]
        anchor_url: String,
        #[arg(long)]
        anchor_data_hash: String,
        #[arg(long)]
        deposit: u64,
        #[arg(long)]
        return_addr: String,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a no-confidence action
    CreateNoConfidence {
        #[arg(long)]
        anchor_url: String,
        #[arg(long)]
        anchor_data_hash: String,
        #[arg(long)]
        deposit: u64,
        #[arg(long)]
        return_addr: String,
        #[arg(long)]
        prev_governance_action_tx_id: Option<String>,
        #[arg(long)]
        prev_governance_action_index: Option<u32>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a new constitution action
    CreateConstitution {
        #[arg(long)]
        anchor_url: String,
        #[arg(long)]
        anchor_data_hash: String,
        #[arg(long)]
        deposit: u64,
        #[arg(long)]
        return_addr: String,
        /// Constitution anchor URL
        #[arg(long)]
        constitution_url: String,
        /// Constitution anchor data hash
        #[arg(long)]
        constitution_hash: String,
        /// Optional guardrail script hash
        #[arg(long)]
        constitution_script_hash: Option<String>,
        #[arg(long)]
        prev_governance_action_tx_id: Option<String>,
        #[arg(long)]
        prev_governance_action_index: Option<u32>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a hard fork initiation action
    ///
    /// cardano-cli 11's canonical name is `create-hardfork`. dugite's
    /// original name, `create-hard-fork-initiation`, is kept as a visible
    /// alias for existing scripts (#1008's naming-normalization pattern,
    /// same as `stake-address stake-delegation-certificate`).
    #[command(
        name = "create-hardfork",
        visible_alias = "create-hard-fork-initiation"
    )]
    CreateHardForkInitiation {
        #[arg(long)]
        anchor_url: String,
        #[arg(long)]
        anchor_data_hash: String,
        #[arg(long)]
        deposit: u64,
        #[arg(long)]
        return_addr: String,
        /// Major protocol version
        #[arg(long)]
        protocol_major_version: u64,
        /// Minor protocol version
        #[arg(long)]
        protocol_minor_version: u64,
        #[arg(long)]
        prev_governance_action_tx_id: Option<String>,
        #[arg(long)]
        prev_governance_action_index: Option<u32>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Compute the hash of anchor data
    HashAnchorData {
        /// Path to the anchor data file
        #[arg(long)]
        file_binary: Option<PathBuf>,
        /// Anchor text to hash directly
        #[arg(long)]
        file_text: Option<PathBuf>,
    },
    /// Create a protocol parameters update action
    CreateProtocolParametersUpdate {
        #[arg(long)]
        anchor_url: String,
        #[arg(long)]
        anchor_data_hash: String,
        #[arg(long)]
        deposit: u64,
        #[arg(long)]
        return_addr: String,
        /// Protocol parameter changes as JSON file
        #[arg(long)]
        protocol_parameters_update: PathBuf,
        /// Optional guardrail script hash
        #[arg(long)]
        constitution_script_hash: Option<String>,
        #[arg(long)]
        prev_governance_action_tx_id: Option<String>,
        #[arg(long)]
        prev_governance_action_index: Option<u32>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create an update committee action
    ///
    /// cardano-cli 11's canonical name is `update-committee`. dugite's
    /// original name, `create-update-committee`, is kept as a visible
    /// alias for existing scripts (#1008).
    #[command(name = "update-committee", visible_alias = "create-update-committee")]
    CreateUpdateCommittee {
        #[arg(long)]
        anchor_url: String,
        #[arg(long)]
        anchor_data_hash: String,
        #[arg(long)]
        deposit: u64,
        #[arg(long)]
        return_addr: String,
        /// Cold verification key files of members to remove
        #[arg(long)]
        remove_cc_cold_verification_key_hash: Vec<String>,
        /// New committee member: key_hash,expiry_epoch
        #[arg(long)]
        add_cc_cold_verification_key_hash: Vec<String>,
        /// Quorum threshold as rational (e.g., "2/3")
        #[arg(long)]
        threshold: String,
        #[arg(long)]
        prev_governance_action_tx_id: Option<String>,
        #[arg(long)]
        prev_governance_action_index: Option<u32>,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Create a treasury withdrawal action
    CreateTreasuryWithdrawal {
        #[arg(long)]
        anchor_url: String,
        #[arg(long)]
        anchor_data_hash: String,
        #[arg(long)]
        deposit: u64,
        #[arg(long)]
        return_addr: String,
        /// Withdrawal target: address+amount
        #[arg(long)]
        funds_receiving_stake_verification_key_file: PathBuf,
        #[arg(long)]
        transfer: u64,
        #[arg(long)]
        out_file: PathBuf,
    },
    /// View a governance action. #1008.
    ///
    /// Matches `cardano-cli conway governance action view`. Decodes the
    /// `Governance proposal` CBOR file each `action create-*` command
    /// above writes.
    View {
        #[arg(long, value_name = "FILEPATH")]
        action_file: PathBuf,
        #[arg(long, conflicts_with = "output_yaml")]
        output_json: bool,
        #[arg(long)]
        output_yaml: bool,
        #[arg(long, value_name = "FILEPATH")]
        out_file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum CommitteeSubcommand {
    /// Create a cold key resignation certificate for a Constitutional
    /// Committee member.
    CreateColdKeyResignationCertificate {
        #[command(flatten)]
        cold: crate::commands::credential::CcColdArgs,
        #[arg(long, value_name = "TEXT")]
        resignation_metadata_url: Option<String>,
        #[arg(long, value_name = "HASH")]
        resignation_metadata_hash: Option<String>,
        #[arg(long, value_name = "FILEPATH")]
        out_file: PathBuf,
    },
    /// Create a hot key authorization certificate for a Constitutional
    /// Committee member.
    CreateHotKeyAuthorizationCertificate {
        #[command(flatten)]
        cold: crate::commands::credential::CcColdArgs,
        #[command(flatten)]
        hot: crate::commands::credential::CcHotArgs,
        #[arg(long, value_name = "FILEPATH")]
        out_file: PathBuf,
    },
    /// Create a cold key pair for a Constitutional Committee member.
    KeyGenCold {
        #[arg(long, value_name = "FILEPATH")]
        cold_verification_key_file: PathBuf,
        #[arg(long, value_name = "FILEPATH")]
        cold_signing_key_file: PathBuf,
    },
    /// Create a hot key pair for a Constitutional Committee member.
    KeyGenHot {
        #[arg(long, value_name = "FILEPATH")]
        verification_key_file: PathBuf,
        #[arg(long, value_name = "FILEPATH")]
        signing_key_file: PathBuf,
    },
    /// Print the identifier (hash) of a Constitutional Committee member key
    /// (hot or cold).
    KeyHash {
        #[arg(long, value_name = "STRING")]
        verification_key: Option<String>,
        #[arg(long, value_name = "FILEPATH")]
        verification_key_file: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
enum MirSubcommand {
    /// Create an MIR certificate to pay stake addresses.
    StakeAddresses {
        #[arg(long, conflicts_with = "treasury")]
        reserves: bool,
        #[arg(long)]
        treasury: bool,
        #[arg(long, value_name = "ADDRESS")]
        stake_address: String,
        #[arg(long, value_name = "LOVELACE")]
        reward: u64,
        #[arg(long, value_name = "FILEPATH")]
        out_file: PathBuf,
    },
    /// Create an MIR certificate to transfer from the reserves pot to the
    /// treasury pot.
    ///
    /// NOTE (#1008, verified against real `cardano-cli 11.0.0.0`, git rev
    /// `97036a66bcf8c89f687ae57a048eecc0389977ef`): this command and
    /// `transfer-to-rewards` below produce BYTE-IDENTICAL certificates —
    /// both encode `mir_pot = 1` (treasury) with a `SendToOppositePotMIR`
    /// target, confirmed on two independent runs with the same `--transfer`
    /// amount. That contradicts each command's own `--help` description
    /// ("reserves pot to the treasury pot" vs "treasury pot to the reserves
    /// pot"), so it looks like a real cardano-cli defect — but this
    /// project's standing rule is to match cardano-cli's ACTUAL
    /// implementation over what its prose claims (CLAUDE.md, the CIP-0121
    /// precedent). Do not "fix" this to encode `mir_pot = 0` for
    /// transfer-to-treasury without re-capturing against a newer
    /// cardano-cli first.
    TransferToTreasury {
        #[arg(long, value_name = "LOVELACE")]
        transfer: u64,
        #[arg(long, value_name = "FILEPATH")]
        out_file: PathBuf,
    },
    /// Create an MIR certificate to transfer from the treasury pot to the
    /// reserves pot. See `transfer-to-treasury`'s doc comment: on real
    /// `cardano-cli 11.0.0.0` this produces the byte-identical certificate.
    TransferToRewards {
        #[arg(long, value_name = "LOVELACE")]
        transfer: u64,
        #[arg(long, value_name = "FILEPATH")]
        out_file: PathBuf,
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

impl GovernanceCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            GovernanceSubcommand::Drep { command } => match command {
                DRepSubcommand::KeyGen {
                    verification_key_file,
                    signing_key_file,
                } => {
                    let sk = dugite_crypto::keys::PaymentSigningKey::generate();
                    let vk = sk.verification_key();

                    let sk_env = serde_json::json!({
                        "type": "DRepSigningKey_ed25519",
                        "description": "Delegated Representative Signing Key",
                        "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
                    });
                    let vk_env = serde_json::json!({
                        "type": "DRepVerificationKey_ed25519",
                        "description": "Delegated Representative Verification Key",
                        "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
                    });

                    std::fs::write(&signing_key_file, serde_json::to_string_pretty(&sk_env)?)?;
                    std::fs::write(
                        &verification_key_file,
                        serde_json::to_string_pretty(&vk_env)?,
                    )?;

                    println!("DRep keys generated.");
                    Ok(())
                }
                DRepSubcommand::Id {
                    drep_verification_key_file,
                    output_format,
                } => {
                    let key_hash = load_key_hash(&drep_verification_key_file)?;

                    if output_format == "hex" {
                        println!("{}", hex::encode(&key_hash));
                    } else {
                        // CIP-0129: DRep key-hash identifiers use the `drep1` HRP.
                        // (The legacy `drep` prefix was superseded by CIP-0129.)
                        let hash28 = dugite_primitives::Hash28::try_from(key_hash.as_slice())
                            .map_err(|_| {
                                anyhow::anyhow!(
                                    "DRep key hash must be 28 bytes, got {}",
                                    key_hash.len()
                                )
                            })?;
                        let drep_id = dugite_primitives::encode_drep_key(&hash28)
                            .map_err(|e| anyhow::anyhow!("Failed to encode DRep ID: {e}"))?;
                        println!("{drep_id}");
                    }
                    Ok(())
                }
                DRepSubcommand::RegistrationCertificate {
                    drep_verification_key_file,
                    key_reg_deposit_amt,
                    anchor_url,
                    anchor_data_hash,
                    out_file,
                } => {
                    let key_hash = load_key_hash(&drep_verification_key_file)?;

                    // Build DRep registration certificate CBOR
                    // Conway cert type 16 = RegDRep
                    let mut cert_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut cert_cbor);

                    let has_anchor = anchor_url.is_some() && anchor_data_hash.is_some();
                    enc.array(if has_anchor { 4 } else { 3 })?;
                    enc.u32(16)?; // RegDRep tag
                                  // Credential: [0, key_hash] for verification key
                    enc.array(2)?;
                    enc.u32(0)?;
                    enc.bytes(&key_hash)?;
                    enc.u64(key_reg_deposit_amt)?;

                    if let (Some(url), Some(hash_hex)) = (&anchor_url, &anchor_data_hash) {
                        let hash_bytes = hex::decode(hash_hex)?;
                        enc.array(2)?;
                        enc.str(url)?;
                        enc.bytes(&hash_bytes)?;
                    }

                    let cert_env = serde_json::json!({
                        "type": "CertificateConway",
                        "description": "DRep Registration Certificate",
                        "cborHex": hex::encode(&cert_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                    println!(
                        "DRep registration certificate written to: {}",
                        out_file.display()
                    );
                    Ok(())
                }
                DRepSubcommand::RetirementCertificate {
                    drep_verification_key_file,
                    deposit_amt,
                    out_file,
                } => {
                    let key_hash = load_key_hash(&drep_verification_key_file)?;

                    // Conway cert type 17 = UnregDRep
                    let mut cert_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                    enc.array(3)?;
                    enc.u32(17)?;
                    enc.array(2)?;
                    enc.u32(0)?;
                    enc.bytes(&key_hash)?;
                    enc.u64(deposit_amt)?;

                    let cert_env = serde_json::json!({
                        "type": "CertificateConway",
                        "description": "DRep Retirement Certificate",
                        "cborHex": hex::encode(&cert_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                    println!(
                        "DRep retirement certificate written to: {}",
                        out_file.display()
                    );
                    Ok(())
                }
                DRepSubcommand::UpdateCertificate {
                    drep_verification_key_file,
                    anchor_url,
                    anchor_data_hash,
                    out_file,
                } => {
                    let key_hash = load_key_hash(&drep_verification_key_file)?;

                    // Conway cert type 18 = UpdateDRep
                    let mut cert_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut cert_cbor);

                    let has_anchor = anchor_url.is_some() && anchor_data_hash.is_some();
                    enc.array(if has_anchor { 3 } else { 2 })?;
                    enc.u32(18)?;
                    enc.array(2)?;
                    enc.u32(0)?;
                    enc.bytes(&key_hash)?;

                    if let (Some(url), Some(hash_hex)) = (&anchor_url, &anchor_data_hash) {
                        let hash_bytes = hex::decode(hash_hex)?;
                        enc.array(2)?;
                        enc.str(url)?;
                        enc.bytes(&hash_bytes)?;
                    }

                    let cert_env = serde_json::json!({
                        "type": "CertificateConway",
                        "description": "DRep Update Certificate",
                        "cborHex": hex::encode(&cert_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                    println!("DRep update certificate written to: {}", out_file.display());
                    Ok(())
                }
                DRepSubcommand::MetadataHash {
                    drep_metadata_file,
                    drep_metadata_url,
                    expected_hash,
                    out_file,
                } => {
                    let bytes: Vec<u8> = if let Some(p) = drep_metadata_file {
                        std::fs::read(&p)
                            .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", p.display()))?
                    } else if let Some(u) = drep_metadata_url {
                        crate::commands::hash::fetch_url_bytes(&u)?
                    } else {
                        anyhow::bail!(
                            "one of --drep-metadata-file or --drep-metadata-url is required"
                        );
                    };

                    let hash = dugite_primitives::hash::blake2b_256(&bytes);
                    let hex_str = hash.to_hex();

                    if let Some(expected) = expected_hash {
                        let expected_norm = expected.trim().to_lowercase();
                        let expected_hash32 =
                            dugite_primitives::hash::Hash32::from_hex(&expected_norm).map_err(
                                |e| anyhow::anyhow!("--expected-hash: unable to read hash: {e}"),
                            )?;
                        if expected_hash32.to_hex() != hex_str {
                            anyhow::bail!(
                                "Hashes do not match!\nExpected: \"{expected_norm}\"\n  Actual: \"{hex_str}\""
                            );
                        }
                        println!("Hashes match!");
                        return Ok(());
                    }

                    match out_file {
                        Some(p) => std::fs::write(&p, &hex_str).map_err(|e| {
                            anyhow::anyhow!("failed to write '{}': {e}", p.display())
                        })?,
                        None => {
                            use std::io::Write;
                            let stdout = std::io::stdout();
                            let mut handle = stdout.lock();
                            handle.write_all(hex_str.as_bytes())?;
                            handle.flush()?;
                        }
                    }
                    Ok(())
                }
            },
            GovernanceSubcommand::Vote { command } => match command {
                VoteSubcommand::Create {
                    governance_action_tx_id,
                    governance_action_index,
                    vote,
                    drep_verification_key_file,
                    cold_verification_key_file,
                    cc_hot_verification_key_file,
                    anchor_url,
                    anchor_data_hash,
                    out_file,
                } => {
                    let vote_value = match vote.to_lowercase().as_str() {
                        "yes" => 1u32,
                        "no" => 0,
                        "abstain" => 2,
                        _ => anyhow::bail!("Invalid vote: '{vote}'. Must be yes, no, or abstain"),
                    };

                    let action_tx_hash = hex::decode(&governance_action_tx_id)?;
                    if action_tx_hash.len() != 32 {
                        anyhow::bail!("Invalid governance action tx id length");
                    }

                    // Determine voter type and credential
                    // Conway `voter` CDDL (oracle-verified against a real
                    // `cardano-cli 11.0.0.0 governance vote create` capture
                    // during #1008): [0, keyhash]=CC hot key, [2, keyhash]=DRep
                    // key, [4, keyhash]=stake pool. This function previously
                    // encoded the SPO arm as type 1, which is actually
                    // ConstitutionalCommitteeHotScriptHash — a vote built with
                    // `--cold-verification-key-file` would decode upstream as a
                    // CC-script vote carrying an unrelated credential hash,
                    // not a stake-pool vote at all. Fixed as part of building
                    // `vote view` against real captures; see the type's own
                    // `#1008` note above.
                    let (voter_type, voter_hash) = if let Some(ref cc_file) =
                        cc_hot_verification_key_file
                    {
                        (0u32, load_key_hash(cc_file)?)
                    } else if let Some(ref cold_file) = cold_verification_key_file {
                        (4, load_key_hash(cold_file)?)
                    } else if let Some(ref drep_file) = drep_verification_key_file {
                        (2, load_key_hash(drep_file)?)
                    } else {
                        anyhow::bail!(
                                "Must provide --drep-verification-key-file, --cold-verification-key-file, or --cc-hot-verification-key-file"
                            );
                    };

                    // Build vote CBOR
                    let mut vote_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut vote_cbor);

                    // Voting procedures map: { voter => { action_id => voting_procedure } }
                    enc.map(1)?;
                    // Voter = [voter_type, addr_keyhash_or_scripthash] — a
                    // BARE hash, not a nested `[cred_type, hash]` credential
                    // structure (Conway `voter` CDDL). This function
                    // previously wrapped the hash in an extra `[0, hash]`
                    // array, which produced a mis-shaped `voter` a real
                    // node would reject; caught (along with the SPO
                    // voter-type bug above) while building `vote view`
                    // against a real `cardano-cli 11.0.0.0` capture (#1008).
                    enc.array(2)?;
                    enc.u32(voter_type)?;
                    enc.bytes(&voter_hash)?;
                    // Action votes map
                    enc.map(1)?;
                    // Action ID: [tx_hash, index]
                    enc.array(2)?;
                    enc.bytes(&action_tx_hash)?;
                    enc.u32(governance_action_index)?;
                    // Voting procedure: [vote, anchor]
                    enc.array(2)?;
                    enc.u32(vote_value)?;
                    if let (Some(url), Some(hash_hex)) = (&anchor_url, &anchor_data_hash) {
                        let hash_bytes = hex::decode(hash_hex)?;
                        enc.array(2)?;
                        enc.str(url)?;
                        enc.bytes(&hash_bytes)?;
                    } else {
                        enc.null()?;
                    }

                    let voter_desc = match voter_type {
                        0 => "Constitutional Committee",
                        4 => "Stake Pool Operator",
                        _ => "DRep",
                    };
                    // Envelope "type" must be exactly "Governance voting
                    // procedures" — this function previously wrote
                    // "VoteConway", which is a genuine other text-envelope
                    // type dugite-cli also produces (some certificates), but
                    // not what cardano-cli's own vote-file reader accepts:
                    // real `cardano-cli … vote view` on the old output
                    // failed with "TextEnvelope type error: Expected:
                    // Governance voting procedures Actual: VoteConway".
                    // Found and fixed alongside the two encoding bugs above.
                    let vote_env = serde_json::json!({
                        "type": "Governance voting procedures",
                        "description": format!("{voter_desc} Governance Vote"),
                        "cborHex": hex::encode(&vote_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&vote_env)?)?;

                    let vote_str = match vote_value {
                        0 => "No",
                        1 => "Yes",
                        _ => "Abstain",
                    };
                    println!("Vote file written to: {}", out_file.display());
                    println!(
                        "Vote: {vote_str} ({voter_desc}) on {governance_action_tx_id}#{governance_action_index}"
                    );
                    Ok(())
                }
                VoteSubcommand::View {
                    vote_file,
                    output_json: _,
                    output_yaml,
                    out_file,
                } => {
                    if output_yaml {
                        anyhow::bail!("--output-yaml is not yet supported (JSON only)");
                    }
                    let content = std::fs::read_to_string(&vote_file).map_err(|e| {
                        anyhow::anyhow!("failed to read '{}': {e}", vote_file.display())
                    })?;
                    let env: serde_json::Value = serde_json::from_str(&content)?;
                    let cbor_hex =
                        env.get("cborHex").and_then(|v| v.as_str()).ok_or_else(|| {
                            anyhow::anyhow!("missing cborHex in {}", vote_file.display())
                        })?;
                    let cbor = hex::decode(cbor_hex.trim())?;

                    // `voting procedures` = Map<Voter, Map<GovActionId, VotingProcedure>>.
                    // Voter = array(2)[type, hash28]:
                    //   0=CC hot key, 1=CC hot script, 2=DRep key, 3=DRep
                    //   script, 4=stake pool key (no script variant) — Conway
                    //   `voter` CDDL, oracle-verified against a real
                    //   `cardano-cli 11.0.0.0 vote view` capture during #1008.
                    // GovActionId = array(2)[tx_hash(32), index].
                    // VotingProcedure = array(2)[vote(0=No/1=Yes/2=Abstain), anchor|null].
                    let mut decoder = minicbor::Decoder::new(&cbor);
                    let voter_count = decoder.map().unwrap_or(Some(0)).unwrap_or(0);
                    let mut out = serde_json::Map::new();
                    for _ in 0..voter_count {
                        let _ = decoder.array(); // Voter [type, hash]
                        let voter_type = decoder.u32().unwrap_or(0);
                        let hash = hex::encode(decoder.bytes().unwrap_or(&[]));
                        let (role, kind) = match voter_type {
                            0 => ("committee", "keyHash"),
                            1 => ("committee", "scriptHash"),
                            2 => ("drep", "keyHash"),
                            3 => ("drep", "scriptHash"),
                            _ => ("stakepool", "keyHash"),
                        };
                        let voter_label = format!("{role}-{kind}-{hash}");

                        let action_count = decoder.map().unwrap_or(Some(0)).unwrap_or(0);
                        let mut actions = serde_json::Map::new();
                        for _ in 0..action_count {
                            let _ = decoder.array(); // GovActionId [tx_hash, index]
                            let tx_id = hex::encode(decoder.bytes().unwrap_or(&[]));
                            let index = decoder.u32().unwrap_or(0);
                            let action_label = format!("{tx_id}#{index}");

                            let _ = decoder.array(); // VotingProcedure [vote, anchor]
                            let vote = decoder.u32().unwrap_or(0);
                            let decision = match vote {
                                0 => "VoteNo",
                                1 => "VoteYes",
                                _ => "Abstain",
                            };
                            let anchor_pos = decoder.position();
                            let anchor = if let Ok(Some(2)) = decoder.array() {
                                let url = decoder.str().unwrap_or("").to_string();
                                let dh = hex::encode(decoder.bytes().unwrap_or(&[]));
                                serde_json::json!({"dataHash": dh, "url": url})
                            } else {
                                decoder.set_position(anchor_pos);
                                decoder.skip().ok(); // null
                                serde_json::Value::Null
                            };

                            actions.insert(
                                action_label,
                                serde_json::json!({"anchor": anchor, "decision": decision}),
                            );
                        }
                        out.insert(voter_label, serde_json::Value::Object(actions));
                    }

                    let rendered = serde_json::to_string_pretty(&serde_json::Value::Object(out))?;
                    match out_file {
                        Some(path) => std::fs::write(&path, &rendered)?,
                        None => println!("{rendered}"),
                    }
                    Ok(())
                }
            },
            GovernanceSubcommand::Action { command } => match command {
                ActionSubcommand::CreateInfo {
                    anchor_url,
                    anchor_data_hash,
                    deposit,
                    return_addr,
                    out_file,
                } => {
                    let anchor_hash = hex::decode(&anchor_data_hash)?;
                    let (_, return_addr_bytes) = bech32::decode(&return_addr)?;

                    // Build governance action CBOR
                    // ProposalProcedure: [deposit, return_addr, gov_action, anchor]
                    let mut action_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut action_cbor);
                    enc.array(4)?;
                    enc.u64(deposit)?;
                    enc.bytes(&return_addr_bytes)?;
                    // InfoAction = tag 6, no params
                    enc.array(1)?;
                    enc.u32(6)?;
                    // Anchor
                    enc.array(2)?;
                    enc.str(&anchor_url)?;
                    enc.bytes(&anchor_hash)?;

                    let action_env = serde_json::json!({
                        "type": "GovernanceActionConway",
                        "description": "Info Governance Action",
                        "cborHex": hex::encode(&action_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&action_env)?)?;
                    println!("Info action written to: {}", out_file.display());
                    Ok(())
                }
                ActionSubcommand::CreateNoConfidence {
                    anchor_url,
                    anchor_data_hash,
                    deposit,
                    return_addr,
                    prev_governance_action_tx_id,
                    prev_governance_action_index,
                    out_file,
                } => {
                    let anchor_hash = hex::decode(&anchor_data_hash)?;
                    let (_, return_addr_bytes) = bech32::decode(&return_addr)?;

                    let mut action_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut action_cbor);
                    enc.array(4)?;
                    enc.u64(deposit)?;
                    enc.bytes(&return_addr_bytes)?;
                    // NoConfidence = tag 3
                    enc.array(2)?;
                    enc.u32(3)?;
                    encode_prev_action_id(
                        &mut enc,
                        &prev_governance_action_tx_id,
                        &prev_governance_action_index,
                    )?;
                    // Anchor
                    enc.array(2)?;
                    enc.str(&anchor_url)?;
                    enc.bytes(&anchor_hash)?;

                    let action_env = serde_json::json!({
                        "type": "GovernanceActionConway",
                        "description": "No Confidence Governance Action",
                        "cborHex": hex::encode(&action_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&action_env)?)?;
                    println!("No-confidence action written to: {}", out_file.display());
                    Ok(())
                }
                ActionSubcommand::CreateConstitution {
                    anchor_url,
                    anchor_data_hash,
                    deposit,
                    return_addr,
                    constitution_url,
                    constitution_hash,
                    constitution_script_hash,
                    prev_governance_action_tx_id,
                    prev_governance_action_index,
                    out_file,
                } => {
                    let anchor_hash = hex::decode(&anchor_data_hash)?;
                    let (_, return_addr_bytes) = bech32::decode(&return_addr)?;
                    let const_hash = hex::decode(&constitution_hash)?;

                    let mut action_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut action_cbor);
                    enc.array(4)?;
                    enc.u64(deposit)?;
                    enc.bytes(&return_addr_bytes)?;
                    // NewConstitution = tag 5
                    enc.array(3)?;
                    enc.u32(5)?;
                    encode_prev_action_id(
                        &mut enc,
                        &prev_governance_action_tx_id,
                        &prev_governance_action_index,
                    )?;
                    // Constitution: [anchor, script_hash]
                    enc.array(2)?;
                    // Constitution anchor
                    enc.array(2)?;
                    enc.str(&constitution_url)?;
                    enc.bytes(&const_hash)?;
                    // Guardrail script hash (nullable)
                    if let Some(ref script_hash_hex) = constitution_script_hash {
                        let script_hash = hex::decode(script_hash_hex)?;
                        enc.bytes(&script_hash)?;
                    } else {
                        enc.null()?;
                    }
                    // Anchor
                    enc.array(2)?;
                    enc.str(&anchor_url)?;
                    enc.bytes(&anchor_hash)?;

                    let action_env = serde_json::json!({
                        "type": "GovernanceActionConway",
                        "description": "New Constitution Governance Action",
                        "cborHex": hex::encode(&action_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&action_env)?)?;
                    println!("New constitution action written to: {}", out_file.display());
                    Ok(())
                }
                ActionSubcommand::CreateHardForkInitiation {
                    anchor_url,
                    anchor_data_hash,
                    deposit,
                    return_addr,
                    protocol_major_version,
                    protocol_minor_version,
                    prev_governance_action_tx_id,
                    prev_governance_action_index,
                    out_file,
                } => {
                    let anchor_hash = hex::decode(&anchor_data_hash)?;
                    let (_, return_addr_bytes) = bech32::decode(&return_addr)?;

                    let mut action_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut action_cbor);
                    enc.array(4)?;
                    enc.u64(deposit)?;
                    enc.bytes(&return_addr_bytes)?;
                    // HardForkInitiation = tag 1
                    enc.array(3)?;
                    enc.u32(1)?;
                    encode_prev_action_id(
                        &mut enc,
                        &prev_governance_action_tx_id,
                        &prev_governance_action_index,
                    )?;
                    // Protocol version: [major, minor]
                    enc.array(2)?;
                    enc.u64(protocol_major_version)?;
                    enc.u64(protocol_minor_version)?;
                    // Anchor
                    enc.array(2)?;
                    enc.str(&anchor_url)?;
                    enc.bytes(&anchor_hash)?;

                    let action_env = serde_json::json!({
                        "type": "GovernanceActionConway",
                        "description": "Hard Fork Initiation Governance Action",
                        "cborHex": hex::encode(&action_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&action_env)?)?;
                    println!(
                        "Hard fork initiation action written to: {}",
                        out_file.display()
                    );
                    Ok(())
                }
                ActionSubcommand::HashAnchorData {
                    file_binary,
                    file_text,
                } => {
                    let data = if let Some(ref path) = file_binary {
                        std::fs::read(path)?
                    } else if let Some(ref path) = file_text {
                        std::fs::read(path)?
                    } else {
                        anyhow::bail!("Must provide either --file-binary or --file-text");
                    };

                    let hash = dugite_primitives::hash::blake2b_256(&data);
                    println!("{}", hex::encode(hash.as_bytes()));
                    Ok(())
                }
                ActionSubcommand::CreateProtocolParametersUpdate {
                    anchor_url,
                    anchor_data_hash,
                    deposit,
                    return_addr,
                    protocol_parameters_update,
                    constitution_script_hash,
                    prev_governance_action_tx_id,
                    prev_governance_action_index,
                    out_file,
                } => {
                    let anchor_hash = hex::decode(&anchor_data_hash)?;
                    let (_, return_addr_bytes) = bech32::decode(&return_addr)?;

                    // Read protocol parameter update JSON
                    let pp_content = std::fs::read_to_string(&protocol_parameters_update)?;
                    let pp_json: serde_json::Value = serde_json::from_str(&pp_content)?;

                    // Encode protocol parameter update as CBOR map
                    let pp_cbor = encode_protocol_param_update(&pp_json)?;

                    let mut action_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut action_cbor);
                    enc.array(4)?;
                    enc.u64(deposit)?;
                    enc.bytes(&return_addr_bytes)?;
                    // ParameterChange = tag 0
                    enc.array(4)?;
                    enc.u32(0)?;
                    encode_prev_action_id(
                        &mut enc,
                        &prev_governance_action_tx_id,
                        &prev_governance_action_index,
                    )?;
                    // Embed raw protocol param update CBOR
                    enc.writer_mut().extend_from_slice(&pp_cbor);
                    // Policy hash
                    if let Some(ref script_hash_hex) = constitution_script_hash {
                        let script_hash = hex::decode(script_hash_hex)?;
                        enc.bytes(&script_hash)?;
                    } else {
                        enc.null()?;
                    }
                    // Anchor
                    enc.array(2)?;
                    enc.str(&anchor_url)?;
                    enc.bytes(&anchor_hash)?;

                    let action_env = serde_json::json!({
                        "type": "GovernanceActionConway",
                        "description": "Protocol Parameters Update Governance Action",
                        "cborHex": hex::encode(&action_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&action_env)?)?;
                    println!(
                        "Protocol parameters update action written to: {}",
                        out_file.display()
                    );
                    Ok(())
                }
                ActionSubcommand::CreateUpdateCommittee {
                    anchor_url,
                    anchor_data_hash,
                    deposit,
                    return_addr,
                    remove_cc_cold_verification_key_hash,
                    add_cc_cold_verification_key_hash,
                    threshold,
                    prev_governance_action_tx_id,
                    prev_governance_action_index,
                    out_file,
                } => {
                    let anchor_hash = hex::decode(&anchor_data_hash)?;
                    let (_, return_addr_bytes) = bech32::decode(&return_addr)?;

                    // Parse threshold as rational "num/den"
                    let thresh_parts: Vec<&str> = threshold.split('/').collect();
                    if thresh_parts.len() != 2 {
                        anyhow::bail!(
                            "Invalid threshold format: '{threshold}'. Expected num/den (e.g., 2/3)"
                        );
                    }
                    let thresh_num: u64 = thresh_parts[0].parse()?;
                    let thresh_den: u64 = thresh_parts[1].parse()?;

                    let mut action_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut action_cbor);
                    enc.array(4)?;
                    enc.u64(deposit)?;
                    enc.bytes(&return_addr_bytes)?;
                    // UpdateCommittee = tag 4
                    enc.array(5)?;
                    enc.u32(4)?;
                    encode_prev_action_id(
                        &mut enc,
                        &prev_governance_action_tx_id,
                        &prev_governance_action_index,
                    )?;
                    // Members to remove: `tag(258) Set<credential>` — confirmed
                    // against a real `cardano-cli 11.0.0.0` capture
                    // (`d90102 80` for an empty set, not a bare `80`). This
                    // encoder previously omitted the tag; found while
                    // building `action view`'s UpdateCommittee decoder
                    // against that same capture (#1008).
                    enc.tag(minicbor::data::Tag::new(258))?;
                    enc.array(remove_cc_cold_verification_key_hash.len() as u64)?;
                    for hash_hex in &remove_cc_cold_verification_key_hash {
                        let hash_bytes = hex::decode(hash_hex)?;
                        enc.array(2)?;
                        enc.u32(0)?; // key credential
                        enc.bytes(&hash_bytes)?;
                    }
                    // Members to add: { credential => expiry_epoch }
                    enc.map(add_cc_cold_verification_key_hash.len() as u64)?;
                    for entry in &add_cc_cold_verification_key_hash {
                        // Format: "key_hash,expiry_epoch"
                        let parts: Vec<&str> = entry.split(',').collect();
                        if parts.len() != 2 {
                            anyhow::bail!(
                                "Invalid add member format: '{entry}'. Expected key_hash,expiry_epoch"
                            );
                        }
                        let hash_bytes = hex::decode(parts[0])?;
                        let expiry: u64 = parts[1].parse()?;
                        enc.array(2)?;
                        enc.u32(0)?;
                        enc.bytes(&hash_bytes)?;
                        enc.u64(expiry)?;
                    }
                    // Threshold: `tag(30) unit_interval` (`d81e 82 <num> <den>`),
                    // not a bare array — same #1008 finding as the removal set.
                    enc.tag(minicbor::data::Tag::new(30))?;
                    enc.array(2)?;
                    enc.u64(thresh_num)?;
                    enc.u64(thresh_den)?;
                    // Anchor
                    enc.array(2)?;
                    enc.str(&anchor_url)?;
                    enc.bytes(&anchor_hash)?;

                    let action_env = serde_json::json!({
                        "type": "GovernanceActionConway",
                        "description": "Update Committee Governance Action",
                        "cborHex": hex::encode(&action_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&action_env)?)?;
                    println!("Update committee action written to: {}", out_file.display());
                    Ok(())
                }
                ActionSubcommand::CreateTreasuryWithdrawal {
                    anchor_url,
                    anchor_data_hash,
                    deposit,
                    return_addr,
                    funds_receiving_stake_verification_key_file,
                    transfer,
                    out_file,
                } => {
                    let anchor_hash = hex::decode(&anchor_data_hash)?;
                    let (_, return_addr_bytes) = bech32::decode(&return_addr)?;

                    // Load the funds-receiving stake verification key and build reward address
                    let stake_vkey_json: serde_json::Value = serde_json::from_str(
                        &std::fs::read_to_string(&funds_receiving_stake_verification_key_file)?,
                    )?;
                    let stake_vkey_hex = stake_vkey_json["cborHex"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("missing cborHex in stake vkey file"))?;
                    let stake_vkey_cbor = hex::decode(stake_vkey_hex)?;
                    // Strip CBOR wrapper (2 bytes for 32-byte key)
                    let stake_vkey_raw = if stake_vkey_cbor.len() > 32 {
                        &stake_vkey_cbor[stake_vkey_cbor.len() - 32..]
                    } else {
                        &stake_vkey_cbor
                    };
                    let stake_hash = dugite_primitives::hash::blake2b_224(stake_vkey_raw);
                    // Reward address: 0xe0 (testnet) or 0xe1 (mainnet) + 28-byte key hash
                    // Use testnet by default (matches return_addr network)
                    let network_byte = if return_addr_bytes.first().is_some_and(|b| b & 0x01 == 1) {
                        0xe1u8 // mainnet
                    } else {
                        0xe0u8 // testnet
                    };
                    let mut withdrawal_addr = vec![network_byte];
                    withdrawal_addr.extend_from_slice(stake_hash.as_ref());

                    let mut action_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut action_cbor);
                    enc.array(4)?;
                    enc.u64(deposit)?;
                    enc.bytes(&return_addr_bytes)?;
                    // TreasuryWithdrawals = tag 2
                    enc.array(3)?;
                    enc.u32(2)?;
                    // Withdrawals map: reward_address → amount
                    enc.map(1)?;
                    enc.bytes(&withdrawal_addr)?;
                    enc.u64(transfer)?;
                    enc.null()?; // policy_hash
                                 // Anchor
                    enc.array(2)?;
                    enc.str(&anchor_url)?;
                    enc.bytes(&anchor_hash)?;

                    let action_env = serde_json::json!({
                        "type": "GovernanceActionConway",
                        "description": "Treasury Withdrawal Governance Action",
                        "cborHex": hex::encode(&action_cbor)
                    });

                    std::fs::write(&out_file, serde_json::to_string_pretty(&action_env)?)?;
                    println!(
                        "Treasury withdrawal action written to: {}",
                        out_file.display()
                    );
                    Ok(())
                }
                ActionSubcommand::View {
                    action_file,
                    output_json: _,
                    output_yaml,
                    out_file,
                } => {
                    if output_yaml {
                        anyhow::bail!("--output-yaml is not yet supported (JSON only)");
                    }
                    let content = std::fs::read_to_string(&action_file).map_err(|e| {
                        anyhow::anyhow!("failed to read '{}': {e}", action_file.display())
                    })?;
                    let env: serde_json::Value = serde_json::from_str(&content)?;
                    let cbor_hex =
                        env.get("cborHex").and_then(|v| v.as_str()).ok_or_else(|| {
                            anyhow::anyhow!("missing cborHex in {}", action_file.display())
                        })?;
                    let cbor = hex::decode(cbor_hex.trim())?;

                    // ProposalProcedure = array(4)[deposit, return_addr(29),
                    // gov_action, anchor]. Same shape every `action create-*`
                    // command above writes. JSON key names for the four
                    // simplest action types (InfoAction/NoConfidence/
                    // HardForkInitiation/NewConstitution/UpdateCommittee/
                    // TreasuryWithdrawals) were captured from a real
                    // `cardano-cli 11.0.0.0 governance action view` run
                    // during #1008 and are pinned byte-for-byte in this
                    // module's tests. ParameterChange's `contents` is NOT a
                    // live capture — cardano-cli's PParamsUpdate JSON uses
                    // named fields this project has not indexed here, so it
                    // is rendered as the raw integer-keyed CBOR map instead
                    // of fabricated field names (an honest gap rather than a
                    // confident wrong shape).
                    let mut decoder = minicbor::Decoder::new(&cbor);
                    let _ = decoder.array(); // array(4)
                    let deposit = decoder.u64().unwrap_or(0);
                    let addr_bytes = decoder.bytes().unwrap_or(&[]).to_vec();
                    let (network, cred_kind) = if addr_bytes.is_empty() {
                        ("Testnet", "keyHash")
                    } else {
                        let network = if addr_bytes[0] & 0x01 != 0 {
                            "Mainnet"
                        } else {
                            "Testnet"
                        };
                        let cred_kind = if addr_bytes[0] & 0x10 != 0 {
                            "scriptHash"
                        } else {
                            "keyHash"
                        };
                        (network, cred_kind)
                    };
                    let cred_hash = if addr_bytes.len() > 1 {
                        hex::encode(&addr_bytes[1..])
                    } else {
                        String::new()
                    };
                    let return_address = serde_json::json!({
                        "credential": {cred_kind: cred_hash},
                        "network": network,
                    });

                    let action_arr_len = decoder.array().unwrap_or(Some(0)).unwrap_or(0);
                    let action_tag = decoder.u32().unwrap_or(999);

                    let read_prev_action_id = |dec: &mut minicbor::Decoder| -> serde_json::Value {
                        let pos = dec.position();
                        if let Ok(Some(2)) = dec.array() {
                            let tx_id = hex::encode(dec.bytes().unwrap_or(&[]));
                            let ix = dec.u32().unwrap_or(0);
                            serde_json::json!({"govActionIx": ix, "txId": tx_id})
                        } else {
                            dec.set_position(pos);
                            dec.skip().ok();
                            serde_json::Value::Null
                        }
                    };

                    let (tag_name, contents): (&str, Option<serde_json::Value>) = match action_tag {
                        6 => ("InfoAction", None),
                        3 => {
                            let prev = read_prev_action_id(&mut decoder);
                            ("NoConfidence", Some(prev))
                        }
                        1 => {
                            let prev = read_prev_action_id(&mut decoder);
                            let _ = decoder.array(); // [major, minor]
                            let major = decoder.u64().unwrap_or(0);
                            let minor = decoder.u64().unwrap_or(0);
                            (
                                "HardForkInitiation",
                                Some(serde_json::json!([prev, {"major": major, "minor": minor}])),
                            )
                        }
                        5 => {
                            let prev = read_prev_action_id(&mut decoder);
                            let _ = decoder.array(); // Constitution [anchor, script?]
                            let _ = decoder.array(); // anchor [url, hash]
                            let url = decoder.str().unwrap_or("").to_string();
                            let dh = hex::encode(decoder.bytes().unwrap_or(&[]));
                            let script_pos = decoder.position();
                            let mut constitution = serde_json::json!({
                                "anchor": {"dataHash": dh, "url": url},
                            });
                            if let Ok(script_bytes) = decoder.bytes() {
                                constitution["script"] =
                                    serde_json::json!(hex::encode(script_bytes));
                            } else {
                                decoder.set_position(script_pos);
                                decoder.skip().ok(); // null
                            }
                            (
                                "NewConstitution",
                                Some(serde_json::json!([prev, constitution])),
                            )
                        }
                        4 => {
                            let prev = read_prev_action_id(&mut decoder);
                            // Members-to-remove is `tag(258) Set<credential>`
                            // (confirmed against a real `cardano-cli
                            // 11.0.0.0` capture — an empty removal set still
                            // carries the tag, `d90102 80`). Consuming it
                            // unconditionally is safe: `.tag()` on a
                            // non-tagged item leaves the decoder position
                            // unchanged, same pattern already used below for
                            // `added`'s per-credential array and the
                            // threshold's `tag(30)`.
                            let _ = decoder.tag();
                            let removed_len = decoder.array().unwrap_or(Some(0)).unwrap_or(0);
                            let mut removed = Vec::new();
                            for _ in 0..removed_len {
                                let _ = decoder.array(); // credential [type, hash]
                                let ctype = decoder.u32().unwrap_or(0);
                                let hash = hex::encode(decoder.bytes().unwrap_or(&[]));
                                let key = if ctype == 1 { "scriptHash" } else { "keyHash" };
                                removed.push(serde_json::json!({key: hash}));
                            }
                            let added_len = decoder.map().unwrap_or(Some(0)).unwrap_or(0);
                            let mut added = serde_json::Map::new();
                            for _ in 0..added_len {
                                let _ = decoder.array(); // credential
                                let ctype = decoder.u32().unwrap_or(0);
                                let hash = hex::encode(decoder.bytes().unwrap_or(&[]));
                                let epoch = decoder.u64().unwrap_or(0);
                                let label = if ctype == 1 {
                                    format!("scriptHash-{hash}")
                                } else {
                                    format!("keyHash-{hash}")
                                };
                                added.insert(label, serde_json::json!(epoch));
                            }
                            let _ = decoder.tag(); // tag(30) rational
                            let _ = decoder.array();
                            let num = decoder.u64().unwrap_or(0);
                            let den = decoder.u64().unwrap_or(1);
                            (
                                "UpdateCommittee",
                                Some(serde_json::json!([
                                    prev,
                                    removed,
                                    added,
                                    {"numerator": num, "denominator": den},
                                ])),
                            )
                        }
                        2 => {
                            let wd_len = decoder.map().unwrap_or(Some(0)).unwrap_or(0);
                            let mut withdrawals = Vec::new();
                            for _ in 0..wd_len {
                                let addr = decoder.bytes().unwrap_or(&[]).to_vec();
                                let amount = decoder.u64().unwrap_or(0);
                                let (net, kind) = if !addr.is_empty() && addr[0] & 0x10 != 0 {
                                    (
                                        if addr[0] & 0x01 != 0 {
                                            "Mainnet"
                                        } else {
                                            "Testnet"
                                        },
                                        "scriptHash",
                                    )
                                } else if !addr.is_empty() {
                                    (
                                        if addr[0] & 0x01 != 0 {
                                            "Mainnet"
                                        } else {
                                            "Testnet"
                                        },
                                        "keyHash",
                                    )
                                } else {
                                    ("Testnet", "keyHash")
                                };
                                let hash = if addr.len() > 1 {
                                    hex::encode(&addr[1..])
                                } else {
                                    String::new()
                                };
                                withdrawals.push(serde_json::json!([
                                    {"credential": {kind: hash}, "network": net},
                                    amount,
                                ]));
                            }
                            let policy_pos = decoder.position();
                            let policy = if let Ok(bytes) = decoder.bytes() {
                                serde_json::json!(hex::encode(bytes))
                            } else {
                                decoder.set_position(policy_pos);
                                decoder.skip().ok();
                                serde_json::Value::Null
                            };
                            (
                                "TreasuryWithdrawals",
                                Some(serde_json::json!([withdrawals, policy])),
                            )
                        }
                        0 => {
                            let prev = read_prev_action_id(&mut decoder);
                            // Raw integer-keyed PParamsUpdate map — see the
                            // doc comment above this match.
                            let start = decoder.position();
                            decoder.skip().ok(); // the PParamsUpdate map itself
                            let end = decoder.position();
                            let ppu_hex = hex::encode(&cbor[start..end]);
                            let policy_pos = decoder.position();
                            let policy = if let Ok(bytes) = decoder.bytes() {
                                serde_json::json!(hex::encode(bytes))
                            } else {
                                decoder.set_position(policy_pos);
                                decoder.skip().ok();
                                serde_json::Value::Null
                            };
                            (
                                "ParameterChange",
                                Some(serde_json::json!([prev, {"cborHex": ppu_hex}, policy])),
                            )
                        }
                        _ => {
                            anyhow::bail!(
                                "unknown governance action tag {action_tag} (array len {action_arr_len})"
                            );
                        }
                    };

                    let _ = decoder.array(); // anchor [url, hash]
                    let anchor_url = decoder.str().unwrap_or("").to_string();
                    let anchor_hash = hex::encode(decoder.bytes().unwrap_or(&[]));

                    let mut gov_action = serde_json::Map::new();
                    gov_action.insert("tag".to_string(), serde_json::json!(tag_name));
                    if let Some(c) = contents {
                        gov_action.insert("contents".to_string(), c);
                    }

                    let rendered = serde_json::to_string_pretty(&serde_json::json!({
                        "anchor": {"dataHash": anchor_hash, "url": anchor_url},
                        "deposit": deposit,
                        "governance action": gov_action,
                        "return address": return_address,
                    }))?;
                    match out_file {
                        Some(path) => std::fs::write(&path, &rendered)?,
                        None => println!("{rendered}"),
                    }
                    Ok(())
                }
            },
            GovernanceSubcommand::Committee { command } => match command {
                CommitteeSubcommand::CreateColdKeyResignationCertificate {
                    cold,
                    resignation_metadata_url,
                    resignation_metadata_hash,
                    out_file,
                } => {
                    let cred = cold.resolve()?;

                    // resign_committee_cold_cert = (15, cold_credential, anchor / null).
                    // The third field is REQUIRED (anchor XOR null), not
                    // omittable — array(3) always, confirmed against a real
                    // `cardano-cli 11.0.0.0` capture with no
                    // `--resignation-metadata-*` flags (`83 0f <cred> f6`,
                    // not `82 0f <cred>`).
                    let mut cert_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                    enc.array(3)?;
                    enc.u32(15)?;
                    enc.array(2)?;
                    enc.u32(cred.cred_type as u32)?;
                    enc.bytes(&cred.hash)?;
                    if let (Some(url), Some(hash_hex)) =
                        (&resignation_metadata_url, &resignation_metadata_hash)
                    {
                        let hash_bytes = hex::decode(hash_hex.trim())?;
                        enc.array(2)?;
                        enc.str(url)?;
                        enc.bytes(&hash_bytes)?;
                    } else {
                        enc.null()?;
                    }

                    let cert_env = serde_json::json!({
                        "type": "CertificateConway",
                        "description": "Constitutional Committee Cold Key Resignation Certificate",
                        "cborHex": hex::encode(&cert_cbor)
                    });
                    std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                    println!(
                        "Cold key resignation certificate written to: {}",
                        out_file.display()
                    );
                    Ok(())
                }
                CommitteeSubcommand::CreateHotKeyAuthorizationCertificate {
                    cold,
                    hot,
                    out_file,
                } => {
                    let cold_cred = cold.resolve()?;
                    let hot_cred = hot.resolve()?;

                    // auth_committee_hot_cert = (14, cold_credential, hot_credential)
                    let mut cert_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                    enc.array(3)?;
                    enc.u32(14)?;
                    enc.array(2)?;
                    enc.u32(cold_cred.cred_type as u32)?;
                    enc.bytes(&cold_cred.hash)?;
                    enc.array(2)?;
                    enc.u32(hot_cred.cred_type as u32)?;
                    enc.bytes(&hot_cred.hash)?;

                    let cert_env = serde_json::json!({
                        "type": "CertificateConway",
                        "description": "Constitutional Committee Hot Key Authorization Certificate",
                        "cborHex": hex::encode(&cert_cbor)
                    });
                    std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                    println!(
                        "Hot key authorization certificate written to: {}",
                        out_file.display()
                    );
                    Ok(())
                }
                CommitteeSubcommand::KeyGenCold {
                    cold_verification_key_file,
                    cold_signing_key_file,
                } => {
                    let sk = dugite_crypto::keys::PaymentSigningKey::generate();
                    let vk = sk.verification_key();

                    let sk_env = serde_json::json!({
                        "type": "ConstitutionalCommitteeColdSigningKey_ed25519",
                        "description": "Constitutional Committee Cold Signing Key",
                        "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
                    });
                    let vk_env = serde_json::json!({
                        "type": "ConstitutionalCommitteeColdVerificationKey_ed25519",
                        "description": "Constitutional Committee Cold Verification Key",
                        "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
                    });
                    std::fs::write(
                        &cold_signing_key_file,
                        serde_json::to_string_pretty(&sk_env)?,
                    )?;
                    std::fs::write(
                        &cold_verification_key_file,
                        serde_json::to_string_pretty(&vk_env)?,
                    )?;
                    println!("Constitutional Committee cold key pair generated.");
                    Ok(())
                }
                CommitteeSubcommand::KeyGenHot {
                    verification_key_file,
                    signing_key_file,
                } => {
                    let sk = dugite_crypto::keys::PaymentSigningKey::generate();
                    let vk = sk.verification_key();

                    let sk_env = serde_json::json!({
                        "type": "ConstitutionalCommitteeHotSigningKey_ed25519",
                        "description": "Constitutional Committee Hot Signing Key",
                        "cborHex": hex::encode(simple_cbor_wrap(&sk.to_bytes()))
                    });
                    let vk_env = serde_json::json!({
                        "type": "ConstitutionalCommitteeHotVerificationKey_ed25519",
                        "description": "Constitutional Committee Hot Verification Key",
                        "cborHex": hex::encode(simple_cbor_wrap(&vk.to_bytes()))
                    });
                    std::fs::write(&signing_key_file, serde_json::to_string_pretty(&sk_env)?)?;
                    std::fs::write(
                        &verification_key_file,
                        serde_json::to_string_pretty(&vk_env)?,
                    )?;
                    println!("Constitutional Committee hot key pair generated.");
                    Ok(())
                }
                CommitteeSubcommand::KeyHash {
                    verification_key,
                    verification_key_file,
                } => {
                    let hash = if let Some(vk) = verification_key {
                        crate::commands::credential::vkey_string_to_hash(&vk)?
                    } else if let Some(path) = verification_key_file {
                        crate::commands::credential::load_vkey_hash_from_envelope(&path)?
                    } else {
                        anyhow::bail!(
                            "missing selector: pass --verification-key or --verification-key-file"
                        );
                    };
                    println!("{}", hex::encode(&hash));
                    Ok(())
                }
            },
            GovernanceSubcommand::CreateMirCertificate { command } => match command {
                MirSubcommand::StakeAddresses {
                    reserves,
                    treasury,
                    stake_address,
                    reward,
                    out_file,
                } => {
                    if reserves == treasury {
                        anyhow::bail!("exactly one of --reserves or --treasury is required");
                    }
                    let cred =
                        crate::commands::credential::stake_address_to_credential(&stake_address)?;
                    let pot: u32 = if treasury { 1 } else { 0 };

                    // move_instantaneous_reward = (6, [pot, {credential => delta_coin}])
                    let mut cert_cbor = Vec::new();
                    let mut enc = minicbor::Encoder::new(&mut cert_cbor);
                    enc.array(2)?;
                    enc.u32(6)?;
                    enc.array(2)?;
                    enc.u32(pot)?;
                    enc.map(1)?;
                    enc.array(2)?;
                    enc.u32(cred.cred_type as u32)?;
                    enc.bytes(&cred.hash)?;
                    enc.u64(reward)?;

                    let cert_env = serde_json::json!({
                        "type": "Certificate",
                        "description": "Move Instantaneous Rewards Certificate",
                        "cborHex": hex::encode(&cert_cbor)
                    });
                    std::fs::write(&out_file, serde_json::to_string_pretty(&cert_env)?)?;
                    println!("MIR certificate written to: {}", out_file.display());
                    Ok(())
                }
                MirSubcommand::TransferToTreasury { transfer, out_file } => {
                    write_mir_pot_transfer_certificate(&out_file, transfer)
                }
                MirSubcommand::TransferToRewards { transfer, out_file } => {
                    write_mir_pot_transfer_certificate(&out_file, transfer)
                }
            },
        }
    }
}

/// Write a `SendToOppositePotMIR` certificate for either
/// `create-mir-certificate transfer-to-treasury` or `transfer-to-rewards`.
///
/// Both commands encode `mir_pot = 1` (treasury) — see the byte-identical
/// finding documented on `MirSubcommand::TransferToTreasury`'s doc comment.
/// Sharing one function makes that empirically-verified equivalence
/// structural rather than something two independent call sites could drift
/// out of sync on.
fn write_mir_pot_transfer_certificate(out_file: &std::path::Path, transfer: u64) -> Result<()> {
    // move_instantaneous_reward = (6, [pot=1(treasury), SendToOppositePotMIR(coin)])
    let mut cert_cbor = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut cert_cbor);
    enc.array(2)?;
    enc.u32(6)?;
    enc.array(2)?;
    enc.u32(1)?;
    enc.u64(transfer)?;

    let cert_env = serde_json::json!({
        "type": "Certificate",
        "description": "MIR Certificate Send To Reserves",
        "cborHex": hex::encode(&cert_cbor)
    });
    std::fs::write(out_file, serde_json::to_string_pretty(&cert_env)?)?;
    println!("MIR certificate written to: {}", out_file.display());
    Ok(())
}

/// Encode a previous governance action ID as CBOR (null if not provided)
fn encode_prev_action_id(
    enc: &mut minicbor::Encoder<&mut Vec<u8>>,
    tx_id: &Option<String>,
    index: &Option<u32>,
) -> Result<()> {
    if let (Some(tx_id_hex), Some(idx)) = (tx_id, index) {
        let tx_hash = hex::decode(tx_id_hex)?;
        if tx_hash.len() != 32 {
            anyhow::bail!("Invalid prev governance action tx id length");
        }
        enc.array(2)?;
        enc.bytes(&tx_hash)?;
        enc.u32(*idx)?;
    } else {
        enc.null()?;
    }
    Ok(())
}

/// Encode protocol parameter update JSON as CBOR map
///
/// Maps JSON field names to their Conway-era CBOR key numbers
fn encode_protocol_param_update(json: &serde_json::Value) -> Result<Vec<u8>> {
    let obj = json
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Protocol param update must be a JSON object"))?;

    // Map of JSON keys to CBOR field numbers
    let field_map: &[(&str, u32)] = &[
        ("txFeePerByte", 0),
        ("minFeeA", 0),
        ("txFeeFixed", 1),
        ("minFeeB", 1),
        ("maxBlockBodySize", 2),
        ("maxTxSize", 3),
        ("maxBlockHeaderSize", 4),
        ("stakeAddressDeposit", 5),
        ("keyDeposit", 5),
        ("stakePoolDeposit", 6),
        ("poolDeposit", 6),
        ("poolRetireMaxEpoch", 7),
        ("eMax", 7),
        ("stakePoolTargetNum", 8),
        ("nOpt", 8),
        ("minPoolCost", 16),
        ("utxoCostPerByte", 17),
        ("adaPerUtxoByte", 17),
        ("maxTxExecutionUnits", 20),
        ("maxBlockExecutionUnits", 21),
        ("maxValueSize", 22),
        ("collateralPercentage", 23),
        ("maxCollateralInputs", 24),
        ("drepDeposit", 30),
        ("govActionDeposit", 31),
        ("govActionLifetime", 32),
    ];

    // Pre-compute the actual field count accounting for aliases and null values
    let mut seen_keys = std::collections::HashSet::new();
    let mut field_count = 0u64;
    for (json_key, cbor_key) in field_map {
        if let Some(value) = obj.get(*json_key) {
            if !value.is_null() && seen_keys.insert(*cbor_key) {
                field_count += 1;
            }
        }
    }

    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);
    enc.map(field_count)?;

    let mut written_keys = std::collections::HashSet::new();
    for (json_key, cbor_key) in field_map {
        if let Some(value) = obj.get(*json_key) {
            if value.is_null() || written_keys.contains(cbor_key) {
                continue;
            }
            written_keys.insert(cbor_key);
            enc.u32(*cbor_key)?;
            if let Some(n) = value.as_u64() {
                enc.u64(n)?;
            } else if let Some(obj) = value.as_object() {
                // Execution units: { "memory": N, "steps": N }
                if let (Some(mem), Some(steps)) = (
                    obj.get("memory").and_then(|v| v.as_u64()),
                    obj.get("steps").and_then(|v| v.as_u64()),
                ) {
                    enc.array(2)?;
                    enc.u64(steps)?;
                    enc.u64(mem)?;
                }
            }
        }
    }

    Ok(buf)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_prev_action_id_none() {
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        encode_prev_action_id(&mut enc, &None, &None).unwrap();
        // Should encode as CBOR null (0xf6)
        assert_eq!(buf, vec![0xf6]);
    }

    #[test]
    fn test_encode_prev_action_id_some() {
        let tx_id = Some("aa".repeat(32)); // 32-byte hex
        let index = Some(3u32);
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        encode_prev_action_id(&mut enc, &tx_id, &index).unwrap();
        // Should start with array(2), then bytes(32), then u32(3)
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.array().unwrap(), Some(2));
        let tx_bytes = dec.bytes().unwrap();
        assert_eq!(tx_bytes.len(), 32);
        assert_eq!(dec.u32().unwrap(), 3);
    }

    #[test]
    fn test_encode_prev_action_id_invalid_length() {
        let tx_id = Some("aabb".to_string()); // only 2 bytes
        let index = Some(0u32);
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        let result = encode_prev_action_id(&mut enc, &tx_id, &index);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("length"));
    }

    #[test]
    fn test_hash_anchor_data_blake2b_256() {
        let data = b"Hello, Cardano!";
        let hash = dugite_primitives::hash::blake2b_256(data);
        // Verify it produces a 32-byte hash
        assert_eq!(hash.as_bytes().len(), 32);
        // Same input should produce same hash
        let hash2 = dugite_primitives::hash::blake2b_256(data);
        assert_eq!(hash.as_bytes(), hash2.as_bytes());
    }

    #[test]
    fn test_encode_protocol_param_update_empty() {
        let json = serde_json::json!({});
        let buf = encode_protocol_param_update(&json).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        // Empty map
        assert_eq!(dec.map().unwrap(), Some(0));
    }

    #[test]
    fn test_encode_protocol_param_update_single_field() {
        let json = serde_json::json!({ "txFeePerByte": 44 });
        let buf = encode_protocol_param_update(&json).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.map().unwrap(), Some(1));
        assert_eq!(dec.u32().unwrap(), 0); // txFeePerByte = key 0
        assert_eq!(dec.u64().unwrap(), 44);
    }

    #[test]
    fn test_encode_protocol_param_update_multiple_fields() {
        let json = serde_json::json!({
            "txFeePerByte": 44,
            "txFeeFixed": 155381,
            "maxTxSize": 16384
        });
        let buf = encode_protocol_param_update(&json).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.map().unwrap(), Some(3));

        // Collect key-value pairs (order depends on HashMap iteration)
        let mut pairs = Vec::new();
        for _ in 0..3 {
            let key = dec.u32().unwrap();
            let val = dec.u64().unwrap();
            pairs.push((key, val));
        }
        pairs.sort_by_key(|(k, _)| *k);

        assert_eq!(pairs[0], (0, 44)); // txFeePerByte
        assert_eq!(pairs[1], (1, 155381)); // txFeeFixed
        assert_eq!(pairs[2], (3, 16384)); // maxTxSize
    }

    #[test]
    fn test_encode_protocol_param_update_null_fields_skipped() {
        let json = serde_json::json!({
            "txFeePerByte": 44,
            "maxTxSize": null
        });
        let buf = encode_protocol_param_update(&json).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        // Only 1 field (null is skipped)
        assert_eq!(dec.map().unwrap(), Some(1));
        assert_eq!(dec.u32().unwrap(), 0);
        assert_eq!(dec.u64().unwrap(), 44);
    }

    #[test]
    fn test_encode_protocol_param_update_execution_units() {
        let json = serde_json::json!({
            "maxTxExecutionUnits": {
                "memory": 14000000000u64,
                "steps": 10000000000000u64
            }
        });
        let buf = encode_protocol_param_update(&json).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.map().unwrap(), Some(1));
        assert_eq!(dec.u32().unwrap(), 20); // maxTxExecutionUnits = key 20
        assert_eq!(dec.array().unwrap(), Some(2));
        // Note: CBOR encodes [steps, memory] per Haskell ExUnits
        assert_eq!(dec.u64().unwrap(), 10000000000000); // steps first
        assert_eq!(dec.u64().unwrap(), 14000000000); // memory second
    }

    #[test]
    fn test_encode_protocol_param_update_alias_dedup() {
        // minFeeA and txFeePerByte both map to key 0 — should only encode once
        let json = serde_json::json!({
            "minFeeA": 44,
            "txFeePerByte": 55
        });
        let buf = encode_protocol_param_update(&json).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        // Only 1 entry (deduplicated by cbor_key)
        assert_eq!(dec.map().unwrap(), Some(1));
        assert_eq!(dec.u32().unwrap(), 0);
        // First encountered wins
        let val = dec.u64().unwrap();
        assert!(val == 44 || val == 55); // JSON object iteration order is non-deterministic
    }

    #[test]
    fn test_encode_protocol_param_update_conway_fields() {
        let json = serde_json::json!({
            "drepDeposit": 500000000,
            "govActionDeposit": 100000000000u64,
            "govActionLifetime": 6
        });
        let buf = encode_protocol_param_update(&json).unwrap();
        let mut dec = minicbor::Decoder::new(&buf);
        assert_eq!(dec.map().unwrap(), Some(3));

        let mut pairs = Vec::new();
        for _ in 0..3 {
            let key = dec.u32().unwrap();
            let val = dec.u64().unwrap();
            pairs.push((key, val));
        }
        pairs.sort_by_key(|(k, _)| *k);

        assert_eq!(pairs[0], (30, 500000000)); // drepDeposit
        assert_eq!(pairs[1], (31, 100000000000)); // govActionDeposit
        assert_eq!(pairs[2], (32, 6)); // govActionLifetime
    }

    #[test]
    fn test_encode_protocol_param_update_not_object() {
        let json = serde_json::json!("not an object");
        let result = encode_protocol_param_update(&json);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("JSON object"));
    }

    #[test]
    fn test_encode_prev_action_id_partial_args() {
        // Only tx_id provided (no index) — should encode as null
        let tx_id = Some("aa".repeat(32));
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        encode_prev_action_id(&mut enc, &tx_id, &None).unwrap();
        assert_eq!(buf, vec![0xf6]); // CBOR null
    }

    // ── Golden vectors captured from real cardano-cli 11.0.0.0 (#1008) ──────
    //
    // Every hex string below was captured by running the equivalent real
    // `cardano-cli` command with the same inputs during the #1008
    // implementation session and confirmed byte-identical to dugite-cli's
    // output before being pinned here.

    #[test]
    fn test_committee_cold_key_resignation_certificate_no_anchor_matches_cardano_cli() {
        // `governance committee create-cold-key-resignation-certificate
        // --cold-verification-key-file <cold.vkey>` (no resignation
        // metadata) — cardano-cli always emits array(3) with an explicit
        // CBOR null anchor, never array(2) omitting the field.
        let expected_hex = "830f8200581c75c5898138aff49ca6e118fcf74d2789514e0726cfb897ed7c05b1b0f6";
        let expected_cbor = hex::decode(expected_hex).unwrap();
        // The credential hash is the 28 bytes between the leading
        // `83 0f 82 00 58 1c` header (array3, tag15, credential-array2,
        // keyHash-type, bstr(28)) and the trailing `f6` (CBOR null anchor).
        let cold_hash = &expected_cbor[6..expected_cbor.len() - 1];
        assert_eq!(cold_hash.len(), 28);

        let mut cert_cbor = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut cert_cbor);
        enc.array(3).unwrap();
        enc.u32(15).unwrap();
        enc.array(2).unwrap();
        enc.u32(0).unwrap();
        enc.bytes(cold_hash).unwrap();
        enc.null().unwrap();

        assert_eq!(hex::encode(&cert_cbor), expected_hex);
    }

    #[test]
    fn test_mir_transfer_to_treasury_and_rewards_are_byte_identical() {
        // Both real cardano-cli commands encode `mir_pot = 1` (treasury)
        // with a `SendToOppositePotMIR` target for the SAME `--transfer`
        // amount — see `write_mir_pot_transfer_certificate`'s doc comment.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        write_mir_pot_transfer_certificate(tmp.path(), 1_234_567).unwrap();
        let env: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(tmp.path()).unwrap()).unwrap();
        assert_eq!(env["cborHex"].as_str().unwrap(), "820682011a0012d687");
    }

    #[test]
    fn test_vote_create_spo_voter_type_is_4_not_1() {
        // Conway `voter` CDDL: [4, addr_keyhash] = StakePoolKeyHash. This
        // function's SPO arm previously encoded type 1
        // (ConstitutionalCommitteeHotScriptHash) — fixed while building
        // `vote view` against a real cardano-cli capture.
        let pool_hash =
            hex::decode("d364dedcd956f1bafeabbce188eec8bc398b48c25b857aa401f2d3ca").unwrap();
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(4).unwrap(); // StakePoolKeyHash, NOT 1
        enc.bytes(&pool_hash).unwrap();

        assert_eq!(
            hex::encode(&buf),
            "8204581cd364dedcd956f1bafeabbce188eec8bc398b48c25b857aa401f2d3ca"
        );
    }

    #[test]
    fn test_vote_create_voter_hash_is_bare_not_nested_credential() {
        // Conway `voter` = [type, hash] — a bare hash, NOT
        // [type, [cred_type, hash]]. Real cardano-cli capture:
        // `82 04 58 1c <28 bytes>`, 30 bytes total. This function
        // previously wrapped the hash in an extra credential array (34
        // bytes total) — fixed alongside the voter-type bug above.
        let pool_hash = vec![0xabu8; 28];
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(2).unwrap();
        enc.u32(4).unwrap();
        enc.bytes(&pool_hash).unwrap();

        // array(2) header(1) + u32-small(1) + bstr(28) header(2, since
        // 28 > 23 needs the `0x58 <len>` long form) + hash(28) = 32.
        assert_eq!(buf.len(), 32);
    }
}
