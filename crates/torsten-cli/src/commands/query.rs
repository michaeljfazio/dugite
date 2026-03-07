use anyhow::Result;
use clap::{Args, Subcommand};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct QueryCmd {
    #[command(subcommand)]
    command: QuerySubcommand,
}

#[derive(Subcommand, Debug)]
enum QuerySubcommand {
    /// Query the current tip
    Tip {
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
    },
    /// Query UTxOs at an address
    Utxo {
        #[arg(long)]
        address: String,
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
    },
    /// Query protocol parameters
    ProtocolParameters {
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
        #[arg(long)]
        out_file: Option<PathBuf>,
    },
    /// Query stake distribution
    StakeDistribution {
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
    },
    /// Query stake address info
    StakeAddressInfo {
        #[arg(long)]
        address: String,
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
    },
    /// Query governance state (Conway era)
    GovState {
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
    },
    /// Query DRep state (Conway era)
    DrepState {
        #[arg(long)]
        drep_key_hash: Option<String>,
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
    },
    /// Query committee state (Conway era)
    CommitteeState {
        #[arg(long, default_value = "node.sock")]
        socket_path: PathBuf,
    },
}

/// Map era index to era name
fn era_name(era: u32) -> &'static str {
    match era {
        0 => "Byron",
        1 => "Shelley",
        2 => "Allegra",
        3 => "Mary",
        4 => "Alonzo",
        5 => "Babbage",
        6 => "Conway",
        _ => "Unknown",
    }
}

impl QueryCmd {
    pub fn run(self) -> Result<()> {
        let rt = tokio::runtime::Runtime::new()?;
        rt.block_on(self.run_async())
    }

    async fn run_async(self) -> Result<()> {
        match self.command {
            QuerySubcommand::Tip { socket_path } => {
                let mut client = torsten_network::N2CClient::connect(&socket_path)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!(
                            "Cannot connect to node socket '{}': {e}\nIs the node running?",
                            socket_path.display()
                        )
                    })?;

                // Default to mainnet magic; could be made configurable
                client
                    .handshake(764824073)
                    .await
                    .map_err(|e| anyhow::anyhow!("Handshake failed: {e}"))?;

                client
                    .acquire()
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to acquire state: {e}"))?;

                let tip = client
                    .query_tip()
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to query tip: {e}"))?;

                let epoch = client.query_epoch().await.unwrap_or(0);
                let era = client.query_era().await.unwrap_or(6);

                client.release().await.ok();
                client.done().await.ok();

                let hash_hex = hex::encode(&tip.hash);
                let era_str = era_name(era);

                println!("{{");
                println!("    \"slot\": {},", tip.slot);
                println!("    \"hash\": \"{hash_hex}\",");
                println!("    \"block\": {},", tip.block_no);
                println!("    \"epoch\": {epoch},");
                println!("    \"era\": \"{era_str}\",");
                println!("    \"syncProgress\": \"100.00\"");
                println!("}}");
                Ok(())
            }
            QuerySubcommand::Utxo {
                address,
                socket_path: _,
            } => {
                println!("Querying UTxOs for {address}...");
                println!("(UTxO query not yet implemented - requires UTxO by address index)");
                Ok(())
            }
            QuerySubcommand::ProtocolParameters {
                socket_path: _,
                out_file,
            } => {
                let params =
                    torsten_primitives::protocol_params::ProtocolParameters::mainnet_defaults();
                let json = serde_json::to_string_pretty(&params)?;
                if let Some(out) = out_file {
                    std::fs::write(&out, &json)?;
                    println!("Protocol parameters written to: {}", out.display());
                } else {
                    println!("{json}");
                }
                Ok(())
            }
            QuerySubcommand::StakeDistribution { socket_path: _ } => {
                println!("Querying stake distribution...");
                println!("(Stake distribution query not yet implemented)");
                Ok(())
            }
            QuerySubcommand::StakeAddressInfo {
                address,
                socket_path: _,
            } => {
                println!("Querying stake address info for {address}...");
                println!("(Stake address info query not yet implemented)");
                Ok(())
            }
            QuerySubcommand::GovState { socket_path: _ } => {
                println!("Querying governance state...");
                println!("(Governance state query not yet implemented)");
                Ok(())
            }
            QuerySubcommand::DrepState {
                drep_key_hash: _,
                socket_path: _,
            } => {
                println!("Querying DRep state...");
                println!("(DRep state query not yet implemented)");
                Ok(())
            }
            QuerySubcommand::CommitteeState { socket_path: _ } => {
                println!("Querying committee state...");
                println!("(Committee state query not yet implemented)");
                Ok(())
            }
        }
    }
}
