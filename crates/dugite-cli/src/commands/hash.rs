//! `hash` command group — cardano-cli's top-level offline hashing utilities.
//!
//! Three subcommands, all offline except `hash anchor-data --url`:
//!   - `hash anchor-data` — generic blake2b-256 of raw bytes (CIP-100/119
//!     anchor data: governance action anchors, DRep metadata, etc.)
//!   - `hash script` — blake2b-224 script hash (native or Plutus V1/V2/V3)
//!   - `hash genesis-file` — blake2b-256 of a genesis JSON file's raw bytes
//!
//! All three exactly mirror `Cardano.CLI.EraIndependent.Hash.Run` in
//! cardano-cli: no JSON canonicalization anywhere, raw bytes in, blake2b out
//! (oracle-verified against cardano-ledger source, cross-checked against a
//! real cardano-cli 11.0.0.0 binary — see crates/dugite-cli/tests/hash_cli.rs).
//!
//! `#1008`/`#1006` CLI surface-parity backlog.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use dugite_primitives::hash::blake2b_256;
use std::io::Write;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct HashCmd {
    #[command(subcommand)]
    command: HashSubcommand,
}

#[derive(Subcommand, Debug)]
enum HashSubcommand {
    /// Compute the hash of some anchor data (to then pass it to other commands)
    AnchorData {
        /// Text to hash as UTF-8
        #[arg(long, conflicts_with_all = ["file_binary", "file_text", "url"])]
        text: Option<String>,
        /// Binary file to hash
        #[arg(long, conflicts_with_all = ["text", "file_text", "url"])]
        file_binary: Option<PathBuf>,
        /// Text file to hash
        #[arg(long, conflicts_with_all = ["text", "file_binary", "url"])]
        file_text: Option<PathBuf>,
        /// A URL to the file to hash (HTTP(S) only; IPFS is not yet supported)
        #[arg(long, conflicts_with_all = ["text", "file_binary", "file_text"])]
        url: Option<String>,
        /// Expected hash for the anchor data, for verification purposes
        #[arg(long, conflicts_with = "out_file")]
        expected_hash: Option<String>,
        #[arg(long)]
        out_file: Option<PathBuf>,
    },
    /// Compute the hash of a script (to then pass it to other commands)
    Script {
        /// Filepath of the script (native-script JSON or a Plutus text envelope)
        #[arg(long)]
        script_file: PathBuf,
        #[arg(long)]
        out_file: Option<PathBuf>,
    },
    /// Compute the hash of a genesis file
    GenesisFile {
        /// The genesis file
        #[arg(long)]
        genesis: PathBuf,
    },
}

/// Write `text` to stdout with NO trailing newline, matching cardano-cli's
/// `hash anchor-data` / `hash script` output exactly (verified via `xxd` on
/// real `cardano-cli 11.0.0.0` output — unlike `hash genesis-file`, which
/// DOES append `\n`; the two code paths genuinely differ upstream).
fn print_no_newline(text: &str) -> Result<()> {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    handle.write_all(text.as_bytes())?;
    handle.flush()?;
    Ok(())
}

/// Fetch `url`'s raw response body bytes (http/https only — IPFS explicitly
/// rejected rather than silently mishandled or guessed at).
///
/// `pub(crate)` so `commands::governance::DRepSubcommand::MetadataHash`
/// (`governance drep metadata-hash`, #1008) can share it: cardano-api's
/// `hashDRepMetadata` and `hashAnnotated (AnchorData bytes)` are two
/// different Haskell code paths but both ultimately hash
/// `blake2b_256(raw bytes)` with zero canonicalization (oracle-verified),
/// so the URL-fetch plumbing has no reason to exist twice.
pub(crate) fn fetch_url_bytes(url: &str) -> Result<Vec<u8>> {
    if url.starts_with("ipfs://") {
        bail!(
            "--url: IPFS URLs are not yet supported by dugite-cli (got '{url}'). \
             Use --file-binary/--file-text with a locally-fetched copy instead."
        );
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        bail!("--url: only http(s) and ipfs URLs are supported, got '{url}'");
    }
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let resp = reqwest::get(url)
            .await
            .map_err(|e| anyhow::anyhow!("failed to fetch '{url}': {e}"))?;
        if !resp.status().is_success() {
            bail!("failed to fetch '{url}': HTTP {}", resp.status());
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow::anyhow!("failed to read response body from '{url}': {e}"))?;
        Ok(bytes.to_vec())
    })
}

/// Compute the canonical Plutus/native script hash for `--script-file`.
///
/// Detects the file shape purely from the JSON `type` field:
///   - `"PlutusScriptV1"`/`"PlutusScriptV2"`/`"PlutusScriptV3"` → a
///     cardano-cli text envelope. `cborHex` is hex-decoded AS-IS (no
///     re-wrapping/unwrapping) and hashed as `blake2b_224(tag || bytes)`.
///     cardano-api's `serialiseToCBOR` for a Plutus script is a deliberate
///     identity over the already-CBOR-bstr-wrapped flat bytes, so `cborHex`
///     IS the exact hash input after the tag byte (oracle-verified against
///     `cardano-ledger-core/.../Plutus/Language.hs` + empirically against a
///     real mainnet script hash AND the vendored plutus-examples fixture;
///     see `hash_cli.rs`). Do NOT strip a CBOR byte-string header here —
///     that produces a DIFFERENT (wrong) hash.
///   - anything else → a native (multisig/timelock) script JSON, the same
///     shape `transaction policyid` accepts. Re-encoded via
///     `encode_native_script` and hashed as `blake2b_224(0x00 || cbor)`.
fn hash_script_file(path: &std::path::Path) -> Result<dugite_primitives::hash::Hash28> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", path.display()))?;
    let json: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("'{}' is not valid JSON: {e}", path.display()))?;

    let type_str = json.get("type").and_then(|v| v.as_str());
    let plutus_tag = match type_str {
        Some("PlutusScriptV1") => Some(1u8),
        Some("PlutusScriptV2") => Some(2u8),
        Some("PlutusScriptV3") => Some(3u8),
        _ => None,
    };

    if let Some(tag) = plutus_tag {
        let cbor_hex = json
            .get("cborHex")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Plutus script envelope missing 'cborHex' field"))?;
        let raw = hex::decode(cbor_hex.trim())
            .map_err(|e| anyhow::anyhow!("invalid 'cborHex' hex: {e}"))?;
        let mut tagged = Vec::with_capacity(1 + raw.len());
        tagged.push(tag);
        tagged.extend_from_slice(&raw);
        return Ok(dugite_primitives::hash::blake2b_224(&tagged));
    }

    // Native script JSON — same parser `transaction policyid` uses.
    let native_script = crate::commands::transaction::parse_json_native_script(&json)?;
    let script_cbor = dugite_serialization::encode::encode_native_script(&native_script);
    let mut tagged = Vec::with_capacity(1 + script_cbor.len());
    tagged.push(0x00);
    tagged.extend_from_slice(&script_cbor);
    Ok(dugite_primitives::hash::blake2b_224(&tagged))
}

impl HashCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            HashSubcommand::AnchorData {
                text,
                file_binary,
                file_text,
                url,
                expected_hash,
                out_file,
            } => {
                let bytes: Vec<u8> = if let Some(t) = text {
                    t.into_bytes()
                } else if let Some(p) = file_binary {
                    std::fs::read(&p)
                        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", p.display()))?
                } else if let Some(p) = file_text {
                    std::fs::read(&p)
                        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", p.display()))?
                } else if let Some(u) = url {
                    fetch_url_bytes(&u)?
                } else {
                    bail!("one of --text, --file-binary, --file-text, or --url is required");
                };

                let hash = blake2b_256(&bytes);
                let hex_str = hash.to_hex();

                if let Some(expected) = expected_hash {
                    let expected_norm = expected.trim().to_lowercase();
                    let expected_hash32 = dugite_primitives::hash::Hash32::from_hex(&expected_norm)
                        .map_err(|e| {
                            anyhow::anyhow!("--expected-hash: unable to read hash: {e}")
                        })?;
                    if expected_hash32.to_hex() != hex_str {
                        bail!(
                            "Hashes do not match!\nExpected: \"{expected_norm}\"\n  Actual: \"{hex_str}\""
                        );
                    }
                    println!("Hashes match!");
                    return Ok(());
                }

                match out_file {
                    Some(p) => std::fs::write(&p, &hex_str)
                        .map_err(|e| anyhow::anyhow!("failed to write '{}': {e}", p.display()))?,
                    None => print_no_newline(&hex_str)?,
                }
                Ok(())
            }
            HashSubcommand::Script {
                script_file,
                out_file,
            } => {
                let hash = hash_script_file(&script_file)?;
                let hex_str = hash.to_hex();
                match out_file {
                    Some(p) => std::fs::write(&p, &hex_str)
                        .map_err(|e| anyhow::anyhow!("failed to write '{}': {e}", p.display()))?,
                    None => print_no_newline(&hex_str)?,
                }
                Ok(())
            }
            HashSubcommand::GenesisFile { genesis } => {
                // cardano-cli hashes the raw file bytes with Blake2b-256 for
                // all non-Byron genesis files (shelley/alonzo/conway) — same
                // logic already proven against real cardano-cli by
                // `genesis hash` (the now-deprecated alias for this exact
                // command); see genesis.rs's `test_genesis_hash_raw_bytes_matches_cardano_cli`.
                // Unlike `hash anchor-data`/`hash script`, cardano-cli DOES
                // print a trailing newline here (verified via `xxd`).
                let raw = std::fs::read(&genesis)
                    .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", genesis.display()))?;
                let hash = blake2b_256(&raw);
                println!("{}", hash.to_hex());
                Ok(())
            }
        }
    }
}
