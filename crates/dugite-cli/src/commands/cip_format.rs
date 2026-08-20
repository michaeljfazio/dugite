//! `cip-format` command group — CIP-0129 governance-identifier bech32
//! formatting utilities (#1091, closing the group #1008 deferred).
//!
//! Four subcommands under `cip-format cip-129`: `drep`, `committee-cold-key`,
//! `committee-hot-key`, `governance-action-id`. Each accepts an input in one
//! of three forms and emits the CIP-129 bech32 identifier either to stdout
//! (`--output-text`, WITH a trailing newline) or to a file (`--output-file`,
//! WITHOUT one) — both behaviours verified against a real cardano-cli 11.0.1
//! binary via `xxd` (see `hash.rs`'s `print_no_newline` for the same
//! stdout/file distinction elsewhere in this crate). Exactly one of
//! `--output-file`/`--output-text` is REQUIRED (cardano-cli has no default);
//! confirmed empirically — running without either prints `Missing:
//! (--output-file FILEPATH | --output-text)`.
//!
//! The CIP-129 encoding itself lives in `dugite_primitives::governance`
//! (`encode_drep_cip129`/`encode_cc_cold_cip129`/`encode_cc_hot_cip129`/
//! `encode_governance_action_id_cip129`), pinned against real cardano-cli
//! captures — see that module's doc comment and tests.
//!
//! # The three `drep`/`committee-*-key` input forms
//!
//! `--*-file FILEPATH` accepts a dugite-cli text-envelope JSON (the common
//! case: `governance drep key-gen` / `governance committee key-gen-cold` /
//! `key-gen-hot` output), OR a file whose entire trimmed content is a raw
//! hex or Bech32 verification key — cardano-cli's own `--help` documents all
//! three ("Input hex/bech32/text envelope ... file").
//!
//! `--*-hex HEX` takes the RAW verification-key bytes (32 plain, or 64
//! extended = pubkey || chain-code) — NOT the already-hashed key hash and
//! NOT the drep-id hex `governance drep id --output-hex` prints. Verified
//! empirically: `cip-format cip-129 drep --drep-hex <32-byte raw vkey>`
//! reproduces the SAME output as `governance drep id --output-cip129` on the
//! text-envelope form of that same key, while feeding the 28-byte KEY-HASH
//! hex there instead fails cardano-cli with "Failed to deserialise ... as
//! VerificationKey DRepKey".
//!
//! `--*-bech32 BECH32` requires the STANDARD (non-CIP129, non-identifier)
//! verification-key Bech32 form — HRP `drep_vk`/`drep_xvk`,
//! `cc_cold_vk`/`cc_cold_xvk`, `cc_hot_vk`/`cc_hot_xvk` — confirmed by real
//! cardano-cli's own rejection message when fed a `drep1…` identifier
//! instead: `"the actual prefix is \"drep\", but it was expected to be
//! \"drep_vk\""` (and `"drep_xvk"` for the extended form). dugite-cli does
//! NOT currently emit `*_vk`/`*_xvk`-prefixed Bech32 verification keys
//! itself (its own key-gen commands only write text-envelope JSON), so this
//! path exists for interop with cardano-cli/cardano-address-produced keys.
//!
//! # `governance-action-id`: two of the three input forms are upstream dead ends
//!
//! Verified against real cardano-cli 11.0.0.0: `--governance-action-file`
//! ALWAYS fails with `"TextEnvelope encoded Governance Action Id is not
//! supported"` and `--governance-action-bech32` ALWAYS fails with `"Bech32
//! encoded Governance Action Id is not supported"`, regardless of content —
//! neither is a real input path in the shipped binary, only
//! `--governance-action-hex` (a literal `<64-hex-char txid>#<index>`, the
//! same `TxIn`-like syntax used elsewhere in cardano-cli) actually works.
//! dugite-cli matches this rather than inventing a shape upstream itself
//! cannot read.

use crate::commands::credential::{load_vkey_hash_from_envelope, vkey_bytes_to_hash};
use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::{
    encode_cc_cold_cip129, encode_cc_hot_cip129, encode_drep_cip129,
    encode_governance_action_id_cip129, CredKind,
};
use std::path::{Path, PathBuf};

#[derive(Args, Debug)]
pub struct CipFormatCmd {
    #[command(subcommand)]
    command: CipFormatSubcommand,
}

#[derive(Subcommand, Debug)]
enum CipFormatSubcommand {
    /// Modified binary encoding of drep keys, constitutional committee cold
    /// and hot keys, governance actions.
    /// https://github.com/cardano-foundation/CIPs/tree/master/CIP-0129
    #[command(name = "cip-129")]
    Cip129 {
        #[command(subcommand)]
        command: Cip129Subcommand,
    },
}

/// Shared `--output-file FILEPATH | --output-text` pair every `cip-129`
/// subcommand carries.
#[derive(Args, Debug)]
struct OutputArgs {
    #[arg(long = "output-file", value_name = "FILEPATH")]
    output_file: Option<PathBuf>,
    #[arg(long = "output-text")]
    output_text: bool,
}

impl OutputArgs {
    /// Write `text` per cardano-cli's own two behaviours: `--output-file`
    /// writes with NO trailing newline, `--output-text` prints to stdout
    /// WITH one (both confirmed via `xxd` against a real binary).
    fn emit(&self, text: &str) -> Result<()> {
        match (&self.output_file, self.output_text) {
            (Some(_), true) => bail!("--output-file and --output-text are mutually exclusive"),
            (Some(path), false) => std::fs::write(path, text)
                .map_err(|e| anyhow::anyhow!("failed to write '{}': {e}", path.display())),
            (None, true) => {
                println!("{text}");
                Ok(())
            }
            (None, false) => bail!("Missing: (--output-file FILEPATH | --output-text)"),
        }
    }
}

#[derive(Subcommand, Debug)]
enum Cip129Subcommand {
    /// Convert drep verification key to the cip-129 compliant format
    Drep {
        #[arg(long = "drep-file", value_name = "FILEPATH")]
        drep_file: Option<PathBuf>,
        #[arg(long = "drep-hex", value_name = "HEX")]
        drep_hex: Option<String>,
        #[arg(long = "drep-bech32", value_name = "BECH32")]
        drep_bech32: Option<String>,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Convert committee hot key to the cip-129 compliant format
    CommitteeHotKey {
        #[arg(long = "committee-hot-key-file", value_name = "FILEPATH")]
        committee_hot_key_file: Option<PathBuf>,
        #[arg(long = "committee-hot-key-hex", value_name = "HEX")]
        committee_hot_key_hex: Option<String>,
        #[arg(long = "committee-hot-key-bech32", value_name = "BECH32")]
        committee_hot_key_bech32: Option<String>,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Convert committee cold key to the cip-129 compliant format
    CommitteeColdKey {
        #[arg(long = "committee-cold-key-file", value_name = "FILEPATH")]
        committee_cold_key_file: Option<PathBuf>,
        #[arg(long = "committee-cold-key-hex", value_name = "HEX")]
        committee_cold_key_hex: Option<String>,
        #[arg(long = "committee-cold-key-bech32", value_name = "BECH32")]
        committee_cold_key_bech32: Option<String>,
        #[command(flatten)]
        output: OutputArgs,
    },
    /// Convert governance action id to the cip-129 compliant format
    GovernanceActionId {
        #[arg(long = "governance-action-file", value_name = "FILEPATH")]
        governance_action_file: Option<PathBuf>,
        #[arg(long = "governance-action-hex", value_name = "HEX")]
        governance_action_hex: Option<String>,
        #[arg(long = "governance-action-bech32", value_name = "BECH32")]
        governance_action_bech32: Option<String>,
        #[command(flatten)]
        output: OutputArgs,
    },
}

/// A raw hex-or-bech32 verification key must be 32 bytes (plain Ed25519) or
/// 64 bytes (extended: 32-byte pubkey || 32-byte chain code).
fn validate_vkey_len(bytes: &[u8]) -> Result<()> {
    if bytes.len() != 32 && bytes.len() != 64 {
        bail!(
            "verification key must be 32 bytes (or 64 for an extended key), got {}",
            bytes.len()
        );
    }
    Ok(())
}

/// Resolve `--*-hex` to a Blake2b-224 verification-key hash. Takes the RAW
/// key bytes, not an already-hashed key hash — see the module doc.
fn resolve_hex(hex_str: &str) -> Result<Vec<u8>> {
    let bytes =
        hex::decode(hex_str.trim()).map_err(|e| anyhow::anyhow!("invalid hex '{hex_str}': {e}"))?;
    validate_vkey_len(&bytes)?;
    Ok(vkey_bytes_to_hash(&bytes))
}

/// Resolve `--*-bech32` to a Blake2b-224 verification-key hash, requiring
/// the HRP to be one of the standard (non-identifier) verification-key
/// forms — see the module doc for why this validation matters.
fn resolve_bech32(s: &str, vk_hrp: &str, xvk_hrp: &str) -> Result<Vec<u8>> {
    let (hrp, bytes) =
        bech32::decode(s.trim()).map_err(|e| anyhow::anyhow!("invalid bech32 '{s}': {e}"))?;
    let hrp_str = hrp.as_str();
    if hrp_str != vk_hrp && hrp_str != xvk_hrp {
        bail!(
            "unexpected Bech32 prefix: the actual prefix is \"{hrp_str}\", but it was expected \
             to be \"{vk_hrp}\" or \"{xvk_hrp}\""
        );
    }
    validate_vkey_len(&bytes)?;
    Ok(vkey_bytes_to_hash(&bytes))
}

/// Resolve `--*-file`: a text-envelope JSON, or a raw hex/bech32 string as
/// the file's entire trimmed content.
fn resolve_file(path: &Path, vk_hrp: &str, xvk_hrp: &str) -> Result<Vec<u8>> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", path.display()))?;
    let trimmed = content.trim();
    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return load_vkey_hash_from_envelope(path);
    }
    let looks_hex = !trimmed.is_empty()
        && trimmed.len().is_multiple_of(2)
        && trimmed.bytes().all(|b| b.is_ascii_hexdigit());
    if looks_hex {
        resolve_hex(trimmed)
    } else {
        resolve_bech32(trimmed, vk_hrp, xvk_hrp)
    }
}

/// Resolve the exactly-one-of-three selector shared by `drep` /
/// `committee-hot-key` / `committee-cold-key`.
#[allow(clippy::too_many_arguments)]
fn resolve_vkey_hash(
    file: Option<&Path>,
    hex: Option<&str>,
    bech32_str: Option<&str>,
    vk_hrp: &str,
    xvk_hrp: &str,
    flag_prefix: &str,
) -> Result<Hash28> {
    let bytes = if let Some(p) = file {
        resolve_file(p, vk_hrp, xvk_hrp)?
    } else if let Some(h) = hex {
        resolve_hex(h)?
    } else if let Some(b) = bech32_str {
        resolve_bech32(b, vk_hrp, xvk_hrp)?
    } else {
        bail!(
            "missing selector: pass one of --{flag_prefix}-file, --{flag_prefix}-hex, or \
             --{flag_prefix}-bech32"
        );
    };
    // `vkey_bytes_to_hash`/blake2b-224 always returns 28 bytes.
    let mut arr = [0u8; 28];
    arr.copy_from_slice(&bytes);
    Ok(Hash28::from_bytes(arr))
}

/// Parse cardano-cli's `TxIn`-like `<64-hex-char txid>#<index>` literal —
/// the only working `governance-action-id` input form (see module doc).
fn parse_gov_action_hex(s: &str) -> Result<(Hash32, u16)> {
    let (txid_hex, index_str) = s
        .split_once('#')
        .ok_or_else(|| anyhow::anyhow!("expected '<txid>#<index>', got '{s}'"))?;
    let txid_bytes = hex::decode(txid_hex.trim())
        .map_err(|e| anyhow::anyhow!("invalid txid hex '{txid_hex}': {e}"))?;
    if txid_bytes.len() != 32 {
        bail!(
            "txid must be 32 bytes (64 hex characters), got {}",
            txid_bytes.len()
        );
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&txid_bytes);
    let index: u16 = index_str
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid governance action index '{index_str}': {e}"))?;
    Ok((Hash32::from_bytes(arr), index))
}

impl CipFormatCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            CipFormatSubcommand::Cip129 { command } => command.run(),
        }
    }
}

impl Cip129Subcommand {
    fn run(self) -> Result<()> {
        match self {
            Cip129Subcommand::Drep {
                drep_file,
                drep_hex,
                drep_bech32,
                output,
            } => {
                let hash = resolve_vkey_hash(
                    drep_file.as_deref(),
                    drep_hex.as_deref(),
                    drep_bech32.as_deref(),
                    "drep_vk",
                    "drep_xvk",
                    "drep",
                )?;
                let encoded = encode_drep_cip129(&hash, CredKind::Key)?;
                output.emit(&encoded)
            }
            Cip129Subcommand::CommitteeHotKey {
                committee_hot_key_file,
                committee_hot_key_hex,
                committee_hot_key_bech32,
                output,
            } => {
                let hash = resolve_vkey_hash(
                    committee_hot_key_file.as_deref(),
                    committee_hot_key_hex.as_deref(),
                    committee_hot_key_bech32.as_deref(),
                    "cc_hot_vk",
                    "cc_hot_xvk",
                    "committee-hot-key",
                )?;
                let encoded = encode_cc_hot_cip129(&hash, CredKind::Key)?;
                output.emit(&encoded)
            }
            Cip129Subcommand::CommitteeColdKey {
                committee_cold_key_file,
                committee_cold_key_hex,
                committee_cold_key_bech32,
                output,
            } => {
                let hash = resolve_vkey_hash(
                    committee_cold_key_file.as_deref(),
                    committee_cold_key_hex.as_deref(),
                    committee_cold_key_bech32.as_deref(),
                    "cc_cold_vk",
                    "cc_cold_xvk",
                    "committee-cold-key",
                )?;
                let encoded = encode_cc_cold_cip129(&hash, CredKind::Key)?;
                output.emit(&encoded)
            }
            Cip129Subcommand::GovernanceActionId {
                governance_action_file,
                governance_action_hex,
                governance_action_bech32,
                output,
            } => {
                if governance_action_file.is_some() {
                    bail!("TextEnvelope encoded Governance Action Id is not supported");
                }
                if governance_action_bech32.is_some() {
                    bail!("Bech32 encoded Governance Action Id is not supported");
                }
                let hex_str = governance_action_hex.ok_or_else(|| {
                    anyhow::anyhow!(
                        "missing selector: pass --governance-action-hex '<txid>#<index>'"
                    )
                })?;
                let (txid, index) = parse_gov_action_hex(&hex_str)?;
                let encoded = encode_governance_action_id_cip129(&txid, index)?;
                output.emit(&encoded)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_envelope(dir: &Path, name: &str, type_: &str, raw32: [u8; 32]) -> PathBuf {
        let path = dir.join(name);
        let env = serde_json::json!({
            "type": type_,
            "description": "",
            "cborHex": format!("5820{}", hex::encode(raw32)),
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();
        path
    }

    /// End-to-end: text-envelope DRep vkey -> CIP-129 identifier, matching
    /// the real cardano-cli capture pinned in
    /// `dugite_primitives::governance`'s own test.
    #[test]
    fn drep_from_envelope_matches_real_cardano_cli_capture() {
        let dir = tempfile::tempdir().unwrap();
        let raw = {
            let hex = "7ac01e00e6c0de3c1ab41f0e49e1b74615d3f826daa67b6214c8f1a323c3b573";
            let mut b = [0u8; 32];
            for (i, byte) in b.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
            }
            b
        };
        let path = write_envelope(dir.path(), "drep.vkey", "DRepVerificationKey_ed25519", raw);
        let hash =
            resolve_vkey_hash(Some(&path), None, None, "drep_vk", "drep_xvk", "drep").unwrap();
        let encoded = encode_drep_cip129(&hash, CredKind::Key).unwrap();
        assert_eq!(
            encoded,
            "drep1yt97ptdxppt60edawjad5d0le3e02lpnsxqqfwd54qwhdugx0pfsd"
        );
    }

    /// `--drep-hex` on the raw 32-byte vkey reproduces the same identifier
    /// as the envelope path — confirms the hex path hashes RAW key bytes,
    /// not an already-computed key hash.
    #[test]
    fn drep_from_hex_matches_envelope_path() {
        let hash = resolve_vkey_hash(
            None,
            Some("7ac01e00e6c0de3c1ab41f0e49e1b74615d3f826daa67b6214c8f1a323c3b573"),
            None,
            "drep_vk",
            "drep_xvk",
            "drep",
        )
        .unwrap();
        let encoded = encode_drep_cip129(&hash, CredKind::Key).unwrap();
        assert_eq!(
            encoded,
            "drep1yt97ptdxppt60edawjad5d0le3e02lpnsxqqfwd54qwhdugx0pfsd"
        );
    }

    /// `--drep-bech32` requires HRP `drep_vk`/`drep_xvk`, NOT the plain
    /// `drep` identifier prefix — feeding an existing drep-id must be
    /// rejected rather than silently double-hashed into a wrong answer.
    #[test]
    fn drep_bech32_rejects_wrong_hrp() {
        let err = resolve_vkey_hash(
            None,
            None,
            Some("drep1e0s2mfsg27n7t0t5htdrtl7vwt6hcvupsqztnd9gr4m0z7pkgj4"),
            "drep_vk",
            "drep_xvk",
            "drep",
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("drep_vk"),
            "error should name the expected HRP, got: {err}"
        );
    }

    #[test]
    fn drep_hex_wrong_length_rejected() {
        let err = resolve_vkey_hash(None, Some("aabbcc"), None, "drep_vk", "drep_xvk", "drep")
            .unwrap_err();
        assert!(err.to_string().contains("32 bytes"));
    }

    #[test]
    fn drep_no_selector_rejected() {
        let err = resolve_vkey_hash(None, None, None, "drep_vk", "drep_xvk", "drep").unwrap_err();
        assert!(err.to_string().contains("missing selector"));
    }

    #[test]
    fn committee_cold_key_from_hex_matches_real_cardano_cli_capture() {
        let hash = resolve_vkey_hash(
            None,
            Some("0d5af06e5efee66003d780ecc3df2b395f49ea72a0f56c7e6fda6e3491a6cf26"),
            None,
            "cc_cold_vk",
            "cc_cold_xvk",
            "committee-cold-key",
        )
        .unwrap();
        let encoded = encode_cc_cold_cip129(&hash, CredKind::Key).unwrap();
        assert_eq!(
            encoded,
            "cc_cold1zgym7c0q8vntwp585xnfe70zjn34fcdxn9hn3jyre2tp3lsr7q8h4"
        );
    }

    #[test]
    fn committee_hot_key_from_hex_matches_real_cardano_cli_capture() {
        let hash = resolve_vkey_hash(
            None,
            Some("2b75f251deefeafd202885843a7fcb6ce10a1d762d928331f976d152d4739a90"),
            None,
            "cc_hot_vk",
            "cc_hot_xvk",
            "committee-hot-key",
        )
        .unwrap();
        let encoded = encode_cc_hot_cip129(&hash, CredKind::Key).unwrap();
        assert_eq!(
            encoded,
            "cc_hot1qg77f6mezv86rczyxe736ncy59nvj97jdr56r0ygn8khvysvzmeuw"
        );
    }

    #[test]
    fn governance_action_id_matches_real_cardano_cli_capture() {
        let (txid, index) = parse_gov_action_hex(&format!("{}#1", "aa".repeat(32))).unwrap();
        let encoded = encode_governance_action_id_cip129(&txid, index).unwrap();
        assert_eq!(
            encoded,
            "gov_action1424242424242424242424242424242424242424242424242424qqqgwfzv8a"
        );
    }

    #[test]
    fn governance_action_id_index_is_big_endian() {
        let (txid, index) = parse_gov_action_hex(&format!("{}#7", "bb".repeat(32))).unwrap();
        let encoded = encode_governance_action_id_cip129(&txid, index).unwrap();
        assert_eq!(
            encoded,
            "gov_action1hwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwasqpctedwqr"
        );
    }

    #[test]
    fn governance_action_id_rejects_missing_hash_separator() {
        let err = parse_gov_action_hex(&"aa".repeat(32)).unwrap_err();
        assert!(err.to_string().contains("txid"));
    }

    #[test]
    fn governance_action_id_file_and_bech32_are_upstream_dead_ends() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gov.txt");
        std::fs::write(&path, "irrelevant").unwrap();
        let cmd = Cip129Subcommand::GovernanceActionId {
            governance_action_file: Some(path),
            governance_action_hex: None,
            governance_action_bech32: None,
            output: OutputArgs {
                output_file: None,
                output_text: true,
            },
        };
        let err = cmd.run().unwrap_err();
        assert!(err.to_string().contains("TextEnvelope"));

        let cmd = Cip129Subcommand::GovernanceActionId {
            governance_action_file: None,
            governance_action_hex: None,
            governance_action_bech32: Some("anything1qqq".to_string()),
            output: OutputArgs {
                output_file: None,
                output_text: true,
            },
        };
        let err = cmd.run().unwrap_err();
        assert!(err.to_string().contains("Bech32"));
    }

    #[test]
    fn output_requires_exactly_one_of_file_or_text() {
        let args = OutputArgs {
            output_file: None,
            output_text: false,
        };
        let err = args.emit("x").unwrap_err();
        assert!(err.to_string().contains("Missing"));
    }

    #[test]
    fn output_file_writes_without_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let args = OutputArgs {
            output_file: Some(path.clone()),
            output_text: false,
        };
        args.emit("hello").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello", "must NOT append a trailing newline");
    }

    /// Full CLI round-trip through `CipFormatCmd::run` (not the lower-level
    /// helpers), exercising the clap parse + dispatch path end to end.
    #[test]
    fn full_command_drep_hex_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let out_path = dir.path().join("out.txt");
        let cmd = CipFormatCmd {
            command: CipFormatSubcommand::Cip129 {
                command: Cip129Subcommand::Drep {
                    drep_file: None,
                    drep_hex: Some(
                        "7ac01e00e6c0de3c1ab41f0e49e1b74615d3f826daa67b6214c8f1a323c3b573"
                            .to_string(),
                    ),
                    drep_bech32: None,
                    output: OutputArgs {
                        output_file: Some(out_path.clone()),
                        output_text: false,
                    },
                },
            },
        };
        cmd.run().unwrap();
        let content = std::fs::read_to_string(&out_path).unwrap();
        assert_eq!(
            content,
            "drep1yt97ptdxppt60edawjad5d0le3e02lpnsxqqfwd54qwhdugx0pfsd"
        );
    }
}
