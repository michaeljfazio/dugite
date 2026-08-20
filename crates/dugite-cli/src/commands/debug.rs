//! `debug` command group (#1091).
//!
//! Two of cardano-cli's three `debug` subcommands: `check-node-configuration`
//! and `transaction view`. `log-epoch-state` remains deferred — see
//! `scripts/validation/cli-surface-known-gaps.txt`.
//!
//! # `debug transaction view` replaces a hand-rolled partial decoder
//!
//! dugite-cli's PRE-EXISTING `transaction view` (`transaction.rs`, kept
//! as-is — it is a documented SUPERSET entry, not this command) is a
//! ~200-line hand-rolled minicbor walker that counts-but-skips certificates
//! and withdrawals and dumps Conway governance fields as `Field N: <present>`
//! instead of decoding them — this project's own recurring N-copies defect
//! pattern (see #1091's issue body). `debug transaction view` instead reuses
//! `dugite_serialization::decode::decode_transaction` — the SAME production
//! decoder the node's own sync/mempool paths use — so every field, including
//! Conway governance (proposals, votes, treasury donations), decodes for
//! real. `Transaction` and its nested types already derive `Serialize`, so
//! the JSON dump is a direct `serde_json::to_value`, not a second hand-built
//! schema.
//!
//! `--tx-body-file` input is a body-only envelope (no witnesses): rather than
//! adding a second body-only decode entry point to `dugite-serialization`,
//! the raw body CBOR bytes are wrapped in a SYNTHETIC standalone-tx array —
//! `[body, {} (empty witness map), true, null]` (or `[body, {}, null]` for
//! Dijkstra, which drops `is_valid` per CIP-0167) — before being handed to
//! the same standalone decoder `--tx-file` uses. The body bytes are decoded
//! by the exact same body-parsing code either way; only the witness set
//! comes back empty, which is correct for a body-only view.
//!
//! `--output-yaml` is NOT implemented (bails clearly) — no YAML serializer
//! is a dependency anywhere in this workspace, and pulling one in for a
//! single deferred-then-closed CLI flag was judged out of proportion to
//! this session's scope. `--output-json` (the documented DEFAULT) is fully
//! implemented.

use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use dugite_primitives::hash::blake2b_256;
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct DebugCmd {
    #[command(subcommand)]
    command: DebugSubcommand,
}

#[derive(Subcommand, Debug)]
enum DebugSubcommand {
    /// Check hashes and paths of genesis files in the given node
    /// configuration file.
    CheckNodeConfiguration {
        #[arg(long = "node-configuration-file", value_name = "FILEPATH")]
        node_configuration_file: PathBuf,
    },
    /// Transaction commands
    Transaction {
        #[command(subcommand)]
        command: DebugTransactionSubcommand,
    },
}

#[derive(Subcommand, Debug)]
enum DebugTransactionSubcommand {
    /// Print a transaction.
    View {
        #[arg(long = "output-json", conflicts_with = "output_yaml")]
        output_json: bool,
        #[arg(long = "output-yaml")]
        output_yaml: bool,
        #[arg(long = "out-file", value_name = "FILEPATH")]
        out_file: Option<PathBuf>,
        #[arg(
            long = "tx-body-file",
            value_name = "FILEPATH",
            conflicts_with = "tx_file"
        )]
        tx_body_file: Option<PathBuf>,
        #[arg(long = "tx-file", value_name = "FILEPATH")]
        tx_file: Option<PathBuf>,
    },
}

impl DebugCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            DebugSubcommand::CheckNodeConfiguration {
                node_configuration_file,
            } => cmd_check_node_configuration(&node_configuration_file),
            DebugSubcommand::Transaction { command } => match command {
                DebugTransactionSubcommand::View {
                    output_yaml,
                    out_file,
                    tx_body_file,
                    tx_file,
                    ..
                } => cmd_view(output_yaml, out_file.as_deref(), tx_body_file, tx_file),
            },
        }
    }
}

/// `debug check-node-configuration` — verify each declared genesis file
/// (Byron/Shelley/Alonzo/Conway) exists at its declared path (resolved
/// relative to the config file's own directory, matching cardano-node's own
/// convention) and its blake2b-256 hash matches the declared
/// `<Era>GenesisHash` field.
///
/// Scoped to the genesis-hash/path check the command's own one-line
/// description promises, NOT a full parse of cardano-node's entire
/// `NodeConfiguration` Aeson schema (hundreds of unrelated fields —
/// confirmed empirically: a real dugite `config.json` that check-node-
/// configuration's OWN genesis-hash fields validate correctly against still
/// fails real cardano-cli 11.0.0.0 with `key "RequiresNetworkMagic" not
/// found`, i.e. cardano-cli's check is coupled to the full schema). Being
/// lenient about fields outside genesis-hash-checking can never produce a
/// false PASS on a genesis mismatch, which is this command's actual job.
///
/// Byron hashes a CANONICAL-JSON re-serialisation of the genesis file, not
/// its raw bytes — confirmed by running this exact check against dugite's
/// own `config/preprod/config.json`: the raw-bytes hash it initially
/// computed (`559db4de…`) did NOT match the declared `ByronGenesisHash`
/// (`d4b8de7a…`), while `dugite-node`'s own `config::byron_genesis_hash`
/// (pinned against three real `cardano-cli byron genesis
/// print-genesis-hash` vectors, `dugite-node/src/config.rs`) computes
/// exactly `d4b8de7a…` for that same file. `write_canonical_json` below is
/// that same algorithm, copied rather than imported because dugite-cli does
/// not depend on dugite-node.
fn cmd_check_node_configuration(config_path: &std::path::Path) -> Result<()> {
    let content = std::fs::read_to_string(config_path)
        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", config_path.display()))?;
    let config: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("'{}' is not valid JSON: {e}", config_path.display()))?;
    let base_dir = config_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."));

    const ERAS: [(&str, &str); 4] = [
        ("Byron", "ByronGenesisFile"),
        ("Shelley", "ShelleyGenesisFile"),
        ("Alonzo", "AlonzoGenesisFile"),
        ("Conway", "ConwayGenesisFile"),
    ];

    let mut any_checked = false;
    let mut any_failed = false;

    for (era_name, file_key) in ERAS {
        let hash_key = format!("{era_name}GenesisHash");
        let Some(file_rel) = config.get(file_key).and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(expected_hash) = config.get(&hash_key).and_then(|v| v.as_str()) else {
            println!("{era_name}: {file_key} present but {hash_key} missing — SKIPPED");
            continue;
        };
        any_checked = true;

        let genesis_path = base_dir.join(file_rel);
        let genesis_bytes = match std::fs::read(&genesis_path) {
            Ok(b) => b,
            Err(e) => {
                any_failed = true;
                println!(
                    "{era_name}: FAIL — could not read '{}': {e}",
                    genesis_path.display()
                );
                continue;
            }
        };
        let actual_hash = if era_name == "Byron" {
            match byron_genesis_hash(&genesis_bytes) {
                Ok(h) => h,
                Err(e) => {
                    any_failed = true;
                    println!(
                        "{era_name}: FAIL — '{}' is not valid JSON: {e}",
                        genesis_path.display()
                    );
                    continue;
                }
            }
        } else {
            blake2b_256(&genesis_bytes).to_hex()
        };
        if actual_hash.eq_ignore_ascii_case(expected_hash.trim()) {
            println!(
                "{era_name}: PASS — '{}' matches {hash_key}",
                genesis_path.display()
            );
        } else {
            any_failed = true;
            println!(
                "{era_name}: FAIL — '{}' hash mismatch: expected {expected_hash}, got {actual_hash}",
                genesis_path.display()
            );
        }
    }

    if !any_checked {
        bail!(
            "no <Era>GenesisFile/<Era>GenesisHash pairs found in '{}'",
            config_path.display()
        );
    }
    if any_failed {
        bail!("one or more genesis file checks failed — see output above");
    }
    Ok(())
}

/// Hash a Byron genesis file the way `cardano-cli byron genesis
/// print-genesis-hash` (and `dugite-node`'s own `config::byron_genesis_hash`)
/// does: a canonical-JSON re-serialisation (keys sorted, no whitespace), NOT
/// the raw file bytes. See `cmd_check_node_configuration`'s doc for how this
/// was confirmed against a real dugite config file.
fn byron_genesis_hash(bytes: &[u8]) -> Result<String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut canonical = String::new();
    write_canonical_json(&value, &mut canonical);
    Ok(blake2b_256(canonical.as_bytes()).to_hex())
}

fn write_canonical_json(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::Value::String((*k).clone()).to_string());
                out.push(':');
                write_canonical_json(&map[*k], out);
            }
            out.push('}');
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (i, v) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical_json(v, out);
            }
            out.push(']');
        }
        // Scalars: serde_json's own compact rendering is already canonical.
        other => out.push_str(&other.to_string()),
    }
}

/// Map an envelope `type` string's era name substring to the HFC era_id
/// `dugite_serialization::decode::decode_transaction` expects. Defaults to
/// Conway (6) when no era name is found, matching
/// `dugite_primitives::transaction::default_era()`'s own fallback.
fn era_id_from_envelope_type(type_str: &str) -> u16 {
    if type_str.contains("Dijkstra") {
        7
    } else if type_str.contains("Conway") {
        6
    } else if type_str.contains("Babbage") {
        5
    } else if type_str.contains("Alonzo") {
        4
    } else if type_str.contains("Mary") {
        3
    } else if type_str.contains("Allegra") {
        2
    } else if type_str.contains("Shelley") {
        1
    } else if type_str.contains("Byron") {
        0
    } else {
        6
    }
}

/// Wrap raw tx-body CBOR bytes in a synthetic standalone-tx array so the
/// same body-parsing code every standalone decoder shares can run on a
/// body-only (`--tx-body-file`) input — see module doc.
fn wrap_body_as_standalone_tx(body_cbor: &[u8], era_id: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(body_cbor.len() + 4);
    if era_id == 7 {
        buf.push(0x83); // array(3): Dijkstra has no is_valid (CIP-0167)
    } else {
        buf.push(0x84); // array(4)
    }
    buf.extend_from_slice(body_cbor);
    buf.push(0xa0); // {} — empty witness-set map
    if era_id != 7 {
        buf.push(0xf5); // true — is_valid
    }
    buf.push(0xf6); // null — no auxiliary data
    buf
}

fn cmd_view(
    output_yaml: bool,
    out_file: Option<&std::path::Path>,
    tx_body_file: Option<PathBuf>,
    tx_file: Option<PathBuf>,
) -> Result<()> {
    if output_yaml {
        bail!("--output-yaml is not yet supported by dugite-cli; use --output-json (the default)");
    }

    let (path, is_body_only) = match (tx_body_file, tx_file) {
        (Some(p), None) => (p, true),
        (None, Some(p)) => (p, false),
        (None, None) => bail!("pass one of --tx-body-file or --tx-file"),
        (Some(_), Some(_)) => unreachable!("clap enforces mutual exclusivity"),
    };

    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("failed to read '{}': {e}", path.display()))?;
    let envelope: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| anyhow::anyhow!("'{}' is not valid JSON: {e}", path.display()))?;
    let type_str = envelope.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let cbor_hex = envelope
        .get("cborHex")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("missing cborHex in '{}'", path.display()))?;
    let cbor_bytes =
        hex::decode(cbor_hex.trim()).map_err(|e| anyhow::anyhow!("invalid cborHex: {e}"))?;

    let era_id = era_id_from_envelope_type(type_str);
    let standalone_cbor = if is_body_only {
        wrap_body_as_standalone_tx(&cbor_bytes, era_id)
    } else {
        cbor_bytes
    };

    let tx = dugite_serialization::decode::decode_transaction(era_id, &standalone_cbor)
        .map_err(|e| anyhow::anyhow!("failed to decode transaction: {e}"))?;
    let json = serde_json::to_string_pretty(&tx)?;

    match out_file {
        Some(p) => std::fs::write(p, &json)
            .map_err(|e| anyhow::anyhow!("failed to write '{}': {e}", p.display()))?,
        None => println!("{json}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn era_id_from_envelope_type_detects_each_era() {
        assert_eq!(era_id_from_envelope_type("Unwitnessed Tx ByronEra"), 0);
        assert_eq!(era_id_from_envelope_type("Witnessed Tx ShelleyEra"), 1);
        assert_eq!(era_id_from_envelope_type("Tx AllegraEra"), 2);
        assert_eq!(era_id_from_envelope_type("Tx MaryEra"), 3);
        assert_eq!(era_id_from_envelope_type("Tx AlonzoEra"), 4);
        assert_eq!(era_id_from_envelope_type("Tx BabbageEra"), 5);
        assert_eq!(era_id_from_envelope_type("Witnessed Tx ConwayEra"), 6);
        assert_eq!(era_id_from_envelope_type("Tx DijkstraEra"), 7);
    }

    #[test]
    fn era_id_from_envelope_type_defaults_to_conway() {
        assert_eq!(era_id_from_envelope_type(""), 6);
        assert_eq!(era_id_from_envelope_type("TxBodyConway"), 6);
    }

    #[test]
    fn wrap_body_as_standalone_tx_produces_array4_for_conway() {
        let body = vec![0xa0u8]; // an empty map stands in for a body
        let wrapped = wrap_body_as_standalone_tx(&body, 6);
        assert_eq!(wrapped, vec![0x84, 0xa0, 0xa0, 0xf5, 0xf6]);
    }

    #[test]
    fn wrap_body_as_standalone_tx_produces_array3_for_dijkstra() {
        let body = vec![0xa0u8];
        let wrapped = wrap_body_as_standalone_tx(&body, 7);
        assert_eq!(wrapped, vec![0x83, 0xa0, 0xa0, 0xf6]);
    }

    fn write_config(dir: &std::path::Path) -> (PathBuf, PathBuf, [u8; 6]) {
        let genesis_bytes = *b"hello!";
        let genesis_path = dir.join("shelley-genesis.json");
        std::fs::write(&genesis_path, genesis_bytes).unwrap();
        let hash = blake2b_256(&genesis_bytes).to_hex();
        let config = serde_json::json!({
            "ShelleyGenesisFile": "shelley-genesis.json",
            "ShelleyGenesisHash": hash,
        });
        let config_path = dir.join("config.json");
        std::fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
        (config_path, genesis_path, genesis_bytes)
    }

    #[test]
    fn check_node_configuration_passes_on_matching_hash() {
        let dir = tempfile::tempdir().unwrap();
        let (config_path, _genesis_path, _bytes) = write_config(dir.path());
        cmd_check_node_configuration(&config_path).unwrap();
    }

    #[test]
    fn check_node_configuration_fails_on_hash_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let genesis_path = dir.path().join("shelley-genesis.json");
        std::fs::write(&genesis_path, b"hello!").unwrap();
        let config = serde_json::json!({
            "ShelleyGenesisFile": "shelley-genesis.json",
            "ShelleyGenesisHash": "0".repeat(64),
        });
        let config_path = dir.path().join("config.json");
        std::fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
        let err = cmd_check_node_configuration(&config_path).unwrap_err();
        assert!(err.to_string().contains("failed"));
    }

    #[test]
    fn check_node_configuration_fails_on_missing_genesis_file() {
        let dir = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "ShelleyGenesisFile": "does-not-exist.json",
            "ShelleyGenesisHash": "0".repeat(64),
        });
        let config_path = dir.path().join("config.json");
        std::fs::write(&config_path, serde_json::to_string(&config).unwrap()).unwrap();
        let err = cmd_check_node_configuration(&config_path).unwrap_err();
        assert!(err.to_string().contains("failed"));
    }

    /// Real preprod Byron genesis, pinned against `cardano-cli byron genesis
    /// print-genesis-hash` — same vector as
    /// `dugite-node`'s `byron_genesis_hash_matches_cardano_node` test. Uses
    /// the repo's real config file rather than a synthetic fixture, since
    /// this is exactly the check that caught the raw-bytes-vs-canonical-JSON
    /// bug during manual testing.
    #[test]
    fn check_node_configuration_hashes_byron_canonically() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("crate is two levels below the repo root");
        let config_path = repo_root.join("config/preprod/config.json");
        if !config_path.exists() {
            // Not every checkout carries the config/ tree (e.g. a crates-only
            // publish) — skip rather than fail on an environment difference.
            return;
        }
        cmd_check_node_configuration(&config_path)
            .expect("preprod's real config.json must pass its own declared genesis hashes");
    }

    #[test]
    fn byron_genesis_hash_matches_real_cardano_cli_vector() {
        let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .expect("crate is two levels below the repo root");
        let genesis_path = repo_root.join("config/preprod/byron-genesis.json");
        if !genesis_path.exists() {
            return;
        }
        let bytes = std::fs::read(&genesis_path).unwrap();
        let hash = byron_genesis_hash(&bytes).unwrap();
        assert_eq!(
            hash, "d4b8de7a11d929a323373cbab6c1a9bdc931beffff11db111cf9d57356ee1937",
            "must match a real `cardano-cli byron genesis print-genesis-hash` capture, \
             NOT blake2b256 of the raw file bytes"
        );
    }

    #[test]
    fn check_node_configuration_errors_with_no_genesis_pairs() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.json");
        std::fs::write(&config_path, "{}").unwrap();
        let err = cmd_check_node_configuration(&config_path).unwrap_err();
        assert!(err.to_string().contains("no"));
    }

    #[test]
    fn view_requires_a_file_selector() {
        let err = cmd_view(false, None, None, None).unwrap_err();
        assert!(err.to_string().contains("pass one of"));
    }

    #[test]
    fn view_rejects_output_yaml() {
        let err = cmd_view(true, None, None, Some(PathBuf::from("/nonexistent"))).unwrap_err();
        assert!(err.to_string().contains("--output-yaml"));
    }

    /// End-to-end: decode a real Conway tx-body envelope (the exact shape
    /// `transaction build-raw --out-file` writes) through the synthetic-
    /// wrapper path and confirm the real fields (inputs/outputs/fee) come
    /// back, not a hand-rolled placeholder.
    #[test]
    fn view_decodes_a_real_tx_body_envelope() {
        // A minimal Conway tx body: {0: [[txid#0]], 1: [[addr, 5000000]], 2: 200000}
        // built the same way `transaction build-raw` constructs one.
        let mut body = Vec::new();
        {
            let mut enc = minicbor::Encoder::new(&mut body);
            enc.map(3).unwrap();
            enc.u8(0).unwrap(); // inputs
            enc.array(1).unwrap();
            enc.array(2).unwrap();
            enc.bytes(&[0xaa; 32]).unwrap();
            enc.u32(0).unwrap();
            enc.u8(1).unwrap(); // outputs
            enc.array(1).unwrap();
            enc.array(2).unwrap();
            enc.bytes(&[0x61; 29]).unwrap(); // a fake 29-byte address
            enc.u64(5_000_000).unwrap();
            enc.u8(2).unwrap(); // fee
            enc.u64(200_000).unwrap();
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tx.body");
        let env = serde_json::json!({
            "type": "TxBodyConway",
            "description": "",
            "cborHex": hex::encode(&body),
        });
        std::fs::write(&path, serde_json::to_string(&env).unwrap()).unwrap();

        let out_path = dir.path().join("out.json");
        cmd_view(false, Some(&out_path), Some(path), None).unwrap();
        let out_content = std::fs::read_to_string(&out_path).unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&out_content).unwrap();
        // The real decoded fee must appear — not a "Field 2: <present>" stub.
        assert_eq!(decoded["body"]["fee"], 200_000);
    }
}
