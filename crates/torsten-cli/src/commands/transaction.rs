use anyhow::{bail, Result};
use clap::{Args, Subcommand};
use std::path::PathBuf;
use torsten_primitives::hash::Hash32;

#[derive(Args, Debug)]
pub struct TransactionCmd {
    #[command(subcommand)]
    command: TxSubcommand,
}

#[derive(Subcommand, Debug)]
enum TxSubcommand {
    /// Build a transaction
    Build {
        /// Transaction inputs (format: tx_hash#index)
        #[arg(long, num_args = 1..)]
        tx_in: Vec<String>,
        /// Transaction outputs (format: address+amount)
        #[arg(long, num_args = 1..)]
        tx_out: Vec<String>,
        /// Change address
        #[arg(long)]
        change_address: String,
        /// Fee amount in lovelace
        #[arg(long, default_value = "200000")]
        fee: u64,
        /// Time-to-live (slot number)
        #[arg(long)]
        ttl: Option<u64>,
        /// Output file for the transaction body
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Sign a transaction
    Sign {
        /// Transaction file to sign
        #[arg(long)]
        tx_body_file: PathBuf,
        /// Signing key files
        #[arg(long, num_args = 1..)]
        signing_key_file: Vec<PathBuf>,
        /// Output file for signed transaction
        #[arg(long)]
        out_file: PathBuf,
    },
    /// Submit a transaction
    Submit {
        /// Signed transaction file
        #[arg(long)]
        tx_file: PathBuf,
        /// Node socket path
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
    },
    /// Calculate transaction hash
    TxId {
        /// Transaction file
        #[arg(long)]
        tx_file: PathBuf,
    },
    /// View transaction contents
    View {
        /// Transaction file
        #[arg(long)]
        tx_file: PathBuf,
    },
}

/// Parse a tx input string "tx_hash#index" into (hash, index)
fn parse_tx_input(s: &str) -> Result<(Hash32, u32)> {
    let parts: Vec<&str> = s.split('#').collect();
    if parts.len() != 2 {
        bail!("Invalid tx input format: '{s}'. Expected tx_hash#index");
    }
    let hash_bytes = hex::decode(parts[0])?;
    if hash_bytes.len() != 32 {
        bail!(
            "Invalid transaction hash length: {} bytes",
            hash_bytes.len()
        );
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&hash_bytes);
    let hash = Hash32::from_bytes(arr);
    let index: u32 = parts[1].parse()?;
    Ok((hash, index))
}

/// Parse a tx output string "address+amount" into (address, lovelace)
fn parse_tx_output(s: &str) -> Result<(String, u64)> {
    let parts: Vec<&str> = s.split('+').collect();
    if parts.len() != 2 {
        bail!("Invalid tx output format: '{s}'. Expected address+amount");
    }
    let address = parts[0].to_string();
    let amount: u64 = parts[1].trim().parse()?;
    Ok((address, amount))
}

/// Build a CBOR transaction body
fn build_tx_body_cbor(
    inputs: &[(Hash32, u32)],
    outputs: &[(String, u64)],
    fee: u64,
    ttl: Option<u64>,
) -> Result<Vec<u8>> {
    let mut buf = Vec::new();
    let mut enc = minicbor::Encoder::new(&mut buf);

    // Transaction body is a map with fields:
    // 0: inputs (set of [tx_hash, index])
    // 1: outputs (array of [address_bytes, amount])
    // 2: fee
    // 3: ttl (optional)
    let field_count = if ttl.is_some() { 4u64 } else { 3 };
    enc.map(field_count)?;

    // Field 0: inputs
    enc.u32(0)?;
    enc.array(inputs.len() as u64)?;
    for (hash, index) in inputs {
        enc.array(2)?;
        enc.bytes(hash.as_bytes())?;
        enc.u32(*index)?;
    }

    // Field 1: outputs
    enc.u32(1)?;
    enc.array(outputs.len() as u64)?;
    for (address, amount) in outputs {
        enc.array(2)?;
        // Decode bech32 address to raw bytes
        let (_hrp, addr_bytes) = bech32::decode(address)
            .map_err(|e| anyhow::anyhow!("Invalid bech32 address '{address}': {e}"))?;
        enc.bytes(&addr_bytes)?;
        enc.u64(*amount)?;
    }

    // Field 2: fee
    enc.u32(2)?;
    enc.u64(fee)?;

    // Field 3: ttl (optional)
    if let Some(ttl_val) = ttl {
        enc.u32(3)?;
        enc.u64(ttl_val)?;
    }

    Ok(buf)
}

impl TransactionCmd {
    pub fn run(self) -> Result<()> {
        match self.command {
            TxSubcommand::Build {
                tx_in,
                tx_out,
                change_address: _,
                fee,
                ttl,
                out_file,
            } => {
                if tx_in.is_empty() {
                    bail!("At least one --tx-in is required");
                }
                if tx_out.is_empty() {
                    bail!("At least one --tx-out is required");
                }

                let inputs: Vec<(Hash32, u32)> = tx_in
                    .iter()
                    .map(|s| parse_tx_input(s))
                    .collect::<Result<_>>()?;
                let outputs: Vec<(String, u64)> = tx_out
                    .iter()
                    .map(|s| parse_tx_output(s))
                    .collect::<Result<_>>()?;

                let tx_body_cbor = build_tx_body_cbor(&inputs, &outputs, fee, ttl)?;

                // Write as text envelope (cardano-cli compatible format)
                let envelope = serde_json::json!({
                    "type": "TxBodyConway",
                    "description": "Transaction Body",
                    "cborHex": hex::encode(&tx_body_cbor)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&envelope)?)?;
                println!("Transaction body written to: {}", out_file.display());
                Ok(())
            }
            TxSubcommand::Sign {
                tx_body_file,
                signing_key_file,
                out_file,
            } => {
                // Read the tx body envelope
                let content = std::fs::read_to_string(&tx_body_file)?;
                let envelope: serde_json::Value = serde_json::from_str(&content)?;
                let tx_body_hex = envelope["cborHex"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing cborHex in tx body file"))?;
                let tx_body_cbor = hex::decode(tx_body_hex)?;

                // Hash the transaction body
                let tx_hash = torsten_crypto::signing::hash_transaction(&tx_body_cbor);

                // Sign with each key
                let mut witnesses = Vec::new();
                for key_file in &signing_key_file {
                    let key_content = std::fs::read_to_string(key_file)?;
                    let key_env: serde_json::Value = serde_json::from_str(&key_content)?;
                    let key_cbor_hex = key_env["cborHex"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Missing cborHex in key file"))?;
                    let key_cbor = hex::decode(key_cbor_hex)?;
                    // Skip CBOR wrapper (2 bytes for byte string header)
                    let key_bytes = if key_cbor.len() > 2 {
                        &key_cbor[2..]
                    } else {
                        &key_cbor
                    };

                    let sk = torsten_crypto::keys::PaymentSigningKey::from_bytes(key_bytes)?;
                    let vk = sk.verification_key();
                    let signature = sk.sign(tx_hash.as_bytes());

                    witnesses.push((vk.to_bytes().to_vec(), signature));
                }

                // Build signed transaction CBOR: [tx_body, witnesses, true, null]
                let mut signed_buf = Vec::new();
                let mut enc = minicbor::Encoder::new(&mut signed_buf);
                enc.array(4)?;

                // Raw tx body CBOR (embed as-is using tag-less bytes)
                // We need to include the raw CBOR, so write it directly
                signed_buf.extend_from_slice(&tx_body_cbor);

                // Re-create encoder after extending
                let mut witness_buf = Vec::new();
                let mut wenc = minicbor::Encoder::new(&mut witness_buf);

                // Witness set: map { 0: [[vkey, sig], ...] }
                wenc.map(1)?;
                wenc.u32(0)?;
                wenc.array(witnesses.len() as u64)?;
                for (vkey, sig) in &witnesses {
                    wenc.array(2)?;
                    wenc.bytes(vkey)?;
                    wenc.bytes(sig)?;
                }

                // Build complete signed tx: [body, witness_set, true, null]
                let mut final_buf = Vec::new();
                let mut fenc = minicbor::Encoder::new(&mut final_buf);
                fenc.array(4)?;
                // Embed raw body CBOR
                final_buf.extend_from_slice(&tx_body_cbor);
                // Embed witness set
                final_buf.extend_from_slice(&witness_buf);
                // is_valid = true
                let mut tail = Vec::new();
                let mut tenc = minicbor::Encoder::new(&mut tail);
                tenc.bool(true)?;
                tenc.null()?;
                final_buf.extend_from_slice(&tail);

                let signed_envelope = serde_json::json!({
                    "type": "Tx ConwayEra",
                    "description": "Signed Transaction",
                    "cborHex": hex::encode(&final_buf)
                });

                std::fs::write(&out_file, serde_json::to_string_pretty(&signed_envelope)?)?;
                println!("Signed transaction written to: {}", out_file.display());
                println!("Transaction hash: {tx_hash}");
                Ok(())
            }
            TxSubcommand::Submit {
                tx_file,
                socket_path,
            } => {
                // Read signed transaction
                let content = std::fs::read_to_string(&tx_file)?;
                let envelope: serde_json::Value = serde_json::from_str(&content)?;
                let cbor_hex = envelope["cborHex"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing cborHex in tx file"))?;
                let tx_cbor = hex::decode(cbor_hex)?;

                // Compute the transaction ID for display
                let body_cbor = extract_tx_body(&tx_cbor)?;
                let tx_hash = torsten_crypto::signing::hash_transaction(&body_cbor);

                // Submit via N2C socket
                let rt = tokio::runtime::Runtime::new()?;
                rt.block_on(async {
                    let mut client = torsten_network::N2CClient::connect(&socket_path)
                        .await
                        .map_err(|e| anyhow::anyhow!("Cannot connect to node socket: {e}"))?;

                    // Handshake (use mainnet magic by default, will be negotiated)
                    client
                        .handshake(764824073)
                        .await
                        .map_err(|e| anyhow::anyhow!("Handshake failed: {e}"))?;

                    // Submit the transaction
                    client
                        .submit_tx(&tx_cbor)
                        .await
                        .map_err(|e| anyhow::anyhow!("{e}"))?;

                    println!("Transaction successfully submitted.");
                    println!("Transaction ID: {tx_hash}");
                    Ok::<(), anyhow::Error>(())
                })?;

                Ok(())
            }
            TxSubcommand::TxId { tx_file } => {
                let content = std::fs::read_to_string(&tx_file)?;
                let envelope: serde_json::Value = serde_json::from_str(&content)?;
                let cbor_hex = envelope["cborHex"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing cborHex in tx file"))?;
                let cbor_bytes = hex::decode(cbor_hex)?;

                // If it's a signed tx [body, witnesses, valid, aux], we need just the body
                // For a tx body file, the whole thing is the body
                let tx_type = envelope["type"].as_str().unwrap_or("");
                let body_cbor = if tx_type.contains("Tx ") || tx_type.contains("Signed") {
                    // Signed tx - extract body from the array
                    // For simplicity, hash the whole body portion
                    // The body is the first element of the CBOR array
                    extract_tx_body(&cbor_bytes)?
                } else {
                    cbor_bytes
                };

                let hash = torsten_crypto::signing::hash_transaction(&body_cbor);
                println!("{hash}");
                Ok(())
            }
            TxSubcommand::View { tx_file } => {
                let content = std::fs::read_to_string(&tx_file)?;
                let envelope: serde_json::Value = serde_json::from_str(&content)?;
                let tx_type = envelope["type"].as_str().unwrap_or("unknown");
                let cbor_hex = envelope["cborHex"].as_str().unwrap_or("");

                println!("Type: {tx_type}");
                println!("CBOR size: {} bytes", cbor_hex.len() / 2);

                let cbor_bytes = hex::decode(cbor_hex)?;
                let body_cbor = if tx_type.contains("Tx ") || tx_type.contains("Signed") {
                    extract_tx_body(&cbor_bytes)?
                } else {
                    cbor_bytes.clone()
                };

                let hash = torsten_crypto::signing::hash_transaction(&body_cbor);
                println!("Transaction hash: {hash}");

                // Try to decode and display basic info
                let mut decoder = minicbor::Decoder::new(&body_cbor);
                if let Ok(Some(map_len)) = decoder.map() {
                    for _ in 0..map_len {
                        if let Ok(key) = decoder.u32() {
                            match key {
                                0 => {
                                    if let Ok(Some(arr_len)) = decoder.array() {
                                        println!("Inputs: {arr_len}");
                                        for _ in 0..arr_len {
                                            decoder.skip().ok();
                                        }
                                    }
                                }
                                1 => {
                                    if let Ok(Some(arr_len)) = decoder.array() {
                                        println!("Outputs: {arr_len}");
                                        for _ in 0..arr_len {
                                            decoder.skip().ok();
                                        }
                                    }
                                }
                                2 => {
                                    if let Ok(fee) = decoder.u64() {
                                        println!("Fee: {fee} lovelace");
                                    }
                                }
                                3 => {
                                    if let Ok(ttl) = decoder.u64() {
                                        println!("TTL: slot {ttl}");
                                    }
                                }
                                _ => {
                                    decoder.skip().ok();
                                }
                            }
                        }
                    }
                }

                Ok(())
            }
        }
    }
}

/// Extract the transaction body CBOR from a signed transaction
/// Signed tx is: [body, witnesses, valid, aux] - we need the body element
fn extract_tx_body(cbor: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = minicbor::Decoder::new(cbor);
    let _ = decoder
        .array()
        .map_err(|e| anyhow::anyhow!("Invalid signed tx CBOR: {e}"))?;

    // Record position before the body
    let body_start = decoder.position();
    decoder
        .skip()
        .map_err(|e| anyhow::anyhow!("Cannot skip tx body: {e}"))?;
    let body_end = decoder.position();

    Ok(cbor[body_start..body_end].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_tx_input_valid() {
        let hash_hex = "a".repeat(64);
        let input = format!("{hash_hex}#0");
        let (hash, index) = parse_tx_input(&input).unwrap();
        assert_eq!(hash, Hash32::from_bytes([0xaa; 32]));
        assert_eq!(index, 0);
    }

    #[test]
    fn test_parse_tx_input_invalid_format() {
        assert!(parse_tx_input("invalid").is_err());
    }

    #[test]
    fn test_parse_tx_output_valid() {
        let (addr, amount) = parse_tx_output("addr_test1abc+5000000").unwrap();
        assert_eq!(addr, "addr_test1abc");
        assert_eq!(amount, 5000000);
    }

    #[test]
    fn test_parse_tx_output_invalid() {
        assert!(parse_tx_output("no_plus_sign").is_err());
    }

    #[test]
    fn test_build_tx_body_cbor() {
        let inputs = vec![(Hash32::from_bytes([0xab; 32]), 0)];
        let outputs = vec![];
        let result = build_tx_body_cbor(&inputs, &outputs, 200000, None);
        // Will fail because no valid bech32 outputs, but at least check with empty outputs
        // Actually with empty outputs it should work fine
        assert!(result.is_ok());
    }

    #[test]
    fn test_extract_tx_body() {
        // Build a simple array: [map{}, map{}, true, null]
        let mut buf = Vec::new();
        let mut enc = minicbor::Encoder::new(&mut buf);
        enc.array(4).unwrap();
        enc.map(0).unwrap(); // body
        enc.map(0).unwrap(); // witnesses
        enc.bool(true).unwrap();
        enc.null().unwrap();

        let body = extract_tx_body(&buf).unwrap();
        // Body should be the CBOR for map(0) = 0xa0
        assert_eq!(body, vec![0xa0]);
    }
}
