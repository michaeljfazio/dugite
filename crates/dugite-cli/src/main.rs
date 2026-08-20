mod commands;

use anyhow::Result;
use clap::Parser;

/// Dugite CLI - Cardano-CLI compatible command-line interface
#[derive(Parser, Debug)]
#[command(name = "dugite-cli", version, about)]
struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

/// # Era-prefix strictness (#935 item 5)
///
/// cardano-cli 11 accepts ONLY the era-prefixed forms (`cardano-cli conway
/// stake-pool ...`) and rejects the bare `stake-pool`/`stake-address`
/// spellings. dugite accepts **both**, which makes it a strict superset:
/// every cardano-cli invocation works unchanged, and the flat forms
/// additionally keep working for existing dugite scripts.
///
/// Decision: keep the leniency. Removing it would break dugite users for no
/// compatibility gain, since accepting a command cardano-cli rejects can never
/// make a cardano-cli-compatible script fail.
///
/// Top-level commands. The era-prefixed variants (`conway`, `babbage`, ...)
/// are aliases for the flat command surface for `cardano-cli` compatibility.
/// All era prefixes currently route to the same handlers — dugite is
/// era-agnostic at the CLI surface today.
#[derive(clap::Subcommand, Debug)]
enum TopCommand {
    #[command(flatten)]
    Flat(Commands),

    /// Conway era commands (alias for top-level subcommands)
    Conway {
        #[command(subcommand)]
        command: Commands,
    },
    /// Babbage era commands (alias for top-level subcommands)
    Babbage {
        #[command(subcommand)]
        command: Commands,
    },
    /// Alonzo era commands (alias for top-level subcommands)
    Alonzo {
        #[command(subcommand)]
        command: Commands,
    },
    /// Mary era commands (alias for top-level subcommands)
    Mary {
        #[command(subcommand)]
        command: Commands,
    },
    /// Allegra era commands (alias for top-level subcommands)
    Allegra {
        #[command(subcommand)]
        command: Commands,
    },
    /// Shelley era commands (alias for top-level subcommands)
    Shelley {
        #[command(subcommand)]
        command: Commands,
    },
    /// Latest era commands (alias for the current era; today: Conway)
    Latest {
        #[command(subcommand)]
        command: Commands,
    },
}

#[derive(clap::Subcommand, Debug)]
enum Commands {
    /// Address commands
    Address(commands::address::AddressCmd),
    /// Byron-era key conversion commands
    Byron(ByronTopCmd),
    /// Key generation commands
    Key(commands::key::KeyCmd),
    /// Transaction commands
    Transaction(Box<commands::transaction::TransactionCmd>),
    /// Query commands
    Query(commands::query::QueryCmd),
    /// Stake address commands
    StakeAddress(commands::stake_address::StakeAddressCmd),
    /// Stake pool commands
    StakePool(commands::stake_pool::StakePoolCmd),
    /// Governance commands (Conway era)
    Governance(commands::governance::GovernanceCmd),
    /// Node-related commands
    Node(commands::node::NodeCmd),
    /// Genesis block commands
    Genesis(commands::genesis::GenesisCmd),
    /// Text-view file commands
    TextView(commands::text_view::TextViewCmd),
    /// Compute hashes to pass to the various --*-hash arguments of commands
    Hash(commands::hash::HashCmd),
    /// CIP-related formatting utilities (CIP-0129 governance identifiers)
    CipFormat(commands::cip_format::CipFormatCmd),
    /// Debug commands
    Debug(commands::debug::DebugCmd),
    /// Ouroboros N2N/N2C connectivity probe
    Ping(commands::ping::PingCmd),
    /// Show the dugite-cli version
    Version,
}

/// `byron key <subcommand>` — mirror the cardano-cli `byron key` nesting.
#[derive(clap::Args, Debug)]
struct ByronTopCmd {
    #[command(subcommand)]
    command: ByronSubcommand,
}

#[derive(clap::Subcommand, Debug)]
enum ByronSubcommand {
    /// Byron key operations
    Key(commands::byron::ByronKeyCmd),
}

impl Commands {
    fn run(self) -> Result<()> {
        match self {
            Commands::Address(cmd) => cmd.run(),
            Commands::Byron(top) => match top.command {
                ByronSubcommand::Key(cmd) => cmd.run(),
            },
            Commands::Key(cmd) => cmd.run(),
            Commands::Transaction(cmd) => cmd.run(),
            Commands::Query(cmd) => cmd.run(),
            Commands::StakeAddress(cmd) => cmd.run(),
            Commands::StakePool(cmd) => cmd.run(),
            Commands::Governance(cmd) => cmd.run(),
            Commands::Node(cmd) => cmd.run(),
            Commands::Genesis(cmd) => cmd.run(),
            Commands::TextView(cmd) => cmd.run(),
            Commands::Hash(cmd) => cmd.run(),
            Commands::CipFormat(cmd) => cmd.run(),
            Commands::Debug(cmd) => cmd.run(),
            Commands::Ping(cmd) => cmd.run(),
            Commands::Version => {
                // Mirrors cardano-cli's `version` subcommand (distinct from
                // its `--version`/`-V` flag, though both print the same
                // string upstream) — dugite reports its OWN identity rather
                // than impersonating cardano-cli's version string. #1008.
                println!("dugite-cli {}", env!("CARGO_PKG_VERSION"));
                Ok(())
            }
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        TopCommand::Flat(cmd)
        | TopCommand::Conway { command: cmd }
        | TopCommand::Babbage { command: cmd }
        | TopCommand::Alonzo { command: cmd }
        | TopCommand::Mary { command: cmd }
        | TopCommand::Allegra { command: cmd }
        | TopCommand::Shelley { command: cmd }
        | TopCommand::Latest { command: cmd } => cmd.run(),
    }
}
