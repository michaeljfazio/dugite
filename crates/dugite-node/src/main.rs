// config and forge are declared in lib.rs and re-used here via the crate root
use dugite_node::config;
use dugite_node::config_reload;
use dugite_node::forge;
mod checkpoints;
mod csj;
mod disk_monitor;
mod genesis;
mod genesis_governor;
mod genesis_peer_state;
mod gsm;
mod leaky_bucket;
mod logging;
mod metrics;
mod mithril;
mod node;
mod rpc_adapter;
mod startup;
mod topology;
mod verify_snapshot;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::info;

/// Dugite - A Rust implementation of the Cardano node
#[derive(Parser, Debug)]
#[command(name = "dugite-node", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Run the node
    Run(Box<RunArgs>),
    /// Import a Mithril snapshot for fast initial sync
    MithrilImport(MithrilImportArgs),
    /// Dump ledger state at epoch boundaries (for cross-validation with cardano-streamer)
    DumpSnapshot(DumpSnapshotArgs),
    /// Database inspection and maintenance tools
    Db(DbArgs),
    /// Compare two ledger snapshots for byte-exact semantic equality
    /// (acceptance harness for issue #670 — Mithril ancillary import).
    ///
    /// Walks every field of the persistent `LedgerStateSnapshot` and
    /// reports mismatches per-field. HashMaps are compared by content
    /// (not by serialised byte order). Use to verify that an
    /// ancillary-imported state equals the state produced by a
    /// from-genesis replay to the same chain tip.
    VerifyLedgerSnapshot(VerifyLedgerSnapshotArgs),
    /// Convert a ledger snapshot between the in-memory and LSM UTxO backends
    /// without a chain replay (the dugite mirror of cardano-node's
    /// `snapshot-converter`). The non-UTxO state is carried over verbatim;
    /// only the UTxO tables are re-encoded. The result is observationally
    /// equivalent to the source (same UTxO set → same ledger → same hashes).
    SnapshotConvert(SnapshotConvertArgs),
}

#[derive(clap::Args, Debug)]
struct SnapshotConvertArgs {
    /// Source database directory containing `ledger-snapshot.bin` (and, for an
    /// LSM source, a `utxo-store/` with a `ledger` snapshot). Read-only; never
    /// modified. Safe to run against a live node's db — the source LSM is read
    /// through its consistent point-in-time `ledger` snapshot.
    #[arg(long)]
    source_db: PathBuf,

    /// Target database directory to write the converted snapshot into
    /// (`ledger-snapshot.bin` + `.meta.json`, plus `utxo-store/` for an LSM
    /// target). Created if absent.
    #[arg(long)]
    target_db: PathBuf,

    /// Target UTxO backend: `mem` (in-memory) or `lsm` (on-disk).
    #[arg(long, value_parser = ["mem", "lsm"])]
    to_backend: String,

    #[command(flatten)]
    log: LogArgs,
}

#[derive(clap::Args, Debug)]
struct VerifyLedgerSnapshotArgs {
    /// First snapshot to compare. May be a `ledger-snapshot.bin` file
    /// or a database directory containing one.
    #[arg(long)]
    left: PathBuf,

    /// Second snapshot to compare. Same format as `--left`.
    #[arg(long)]
    right: PathBuf,

    /// Print side-by-side scalar overview before reporting diffs.
    /// Useful for triage when the diff fields require context.
    #[arg(long)]
    verbose: bool,

    /// Print the first N pool_params entries that differ. Surfaces
    /// the structural shape of the divergence inside `PoolRegistration`.
    #[arg(long, default_value_t = 0)]
    show_pool_diffs: usize,

    #[command(flatten)]
    log: LogArgs,
}

#[derive(clap::Args, Debug)]
struct DbArgs {
    #[command(subcommand)]
    command: DbCommand,
}

#[derive(clap::Subcommand, Debug)]
enum DbCommand {
    /// Show database size and block count information
    Info(DbInfoArgs),
}

#[derive(clap::Args, Debug)]
struct DbInfoArgs {
    /// Path to the database directory
    #[arg(long, default_value = "db")]
    database_path: PathBuf,

    /// Storage profile: ultra-memory, high-memory (default), low-memory, or minimal
    #[arg(long, default_value = "high-memory")]
    storage_profile: String,
}

/// Shared logging arguments for all subcommands
#[derive(clap::Args, Debug, Clone)]
struct LogArgs {
    /// Log output targets: stdout, file, journald (can specify multiple)
    #[arg(long = "log-output", default_value = "stdout")]
    log_outputs: Vec<String>,

    /// Log level (trace, debug, info, warn, error). Overridden by RUST_LOG env var.
    #[arg(long)]
    log_level: Option<String>,

    /// Directory for log files (used with --log-output file)
    #[arg(long, default_value = "logs")]
    log_dir: PathBuf,

    /// Log output format: text (human-readable) or json (structured)
    #[arg(long, default_value = "text")]
    log_format: String,

    /// Log file rotation strategy: daily, hourly, never
    #[arg(long, default_value = "daily")]
    log_file_rotation: String,

    /// Disable ANSI colors in stdout output
    #[arg(long)]
    log_no_color: bool,

    /// Number of days to retain log files (default: 7)
    #[arg(long, default_value = "7")]
    log_retention_days: u64,

    /// Channel-full policy for the non-blocking stdout writer (issue #650).
    ///
    /// `drop` (default) — under flood the producer keeps going and dropped
    /// lines are counted; matches `tracing_appender` upstream default.
    /// `block` — producer parks until the worker drains; lossless, but
    /// re-introduces the blocking behavior on the hot path.
    #[arg(long, default_value = "drop")]
    stdout_overflow: String,
}

#[derive(clap::Args, Debug)]
struct RunArgs {
    /// Path to the node configuration file
    #[arg(long, default_value = "config/mainnet/config.json")]
    config: PathBuf,

    /// Path to the topology file
    #[arg(long, default_value = "config/mainnet/topology.json")]
    topology: PathBuf,

    /// Path to the database directory
    #[arg(long, default_value = "db")]
    database_path: PathBuf,

    /// Unix domain socket path for local clients
    #[arg(long, default_value = "node.sock")]
    socket_path: PathBuf,

    /// TCP port for node-to-node connections
    #[arg(long, default_value = "3001")]
    port: u16,

    /// Host address to bind to
    #[arg(long, default_value = "0.0.0.0")]
    host_addr: String,

    /// Prometheus metrics port.
    ///
    /// Overrides the MetricsPort value from the config file.
    /// Pass 0 to disable the metrics server.
    /// If not specified, the config file value is used; if neither is set,
    /// the default port 12796 is used.
    #[arg(long)]
    metrics_port: Option<u16>,

    /// Disable the Prometheus metrics server entirely.
    ///
    /// Equivalent to `--metrics-port 0`. Takes precedence over `--metrics-port`
    /// and the MetricsPort config file field.
    #[arg(long)]
    no_metrics: bool,

    /// Make a metrics bind failure a fatal startup error.
    ///
    /// By default the node continues if the Prometheus metrics port cannot be
    /// bound (e.g. already in use).  With this flag a bind failure causes the
    /// node to exit with a non-zero status instead.  Useful in supervised
    /// deployments where a missing metrics endpoint must be treated as a hard
    /// failure rather than a silent degradation.
    #[arg(long)]
    require_metrics: bool,

    /// UTxO RPC (gRPC) server bind address (issue #672).
    ///
    /// Overrides the `Rpc.ListenAddr` value from the config file. Defaults
    /// to `127.0.0.1` when the server is enabled.
    #[arg(long)]
    rpc_host: Option<String>,

    /// UTxO RPC (gRPC) server port (issue #672).
    ///
    /// Overrides the `Rpc.Port` value from the config file. Implies
    /// enabling the RPC server. Defaults to `50051` when set via the
    /// config file. Pass `--no-rpc` to disable.
    #[arg(long)]
    rpc_port: Option<u16>,

    /// Disable the UTxO RPC (gRPC) server entirely (issue #672).
    ///
    /// Takes precedence over `--rpc-host`, `--rpc-port`, and the
    /// `Rpc.Enabled` config-file field.
    #[arg(long)]
    no_rpc: bool,

    /// Also emit `cardano_node_metrics_*` compatibility aliases in the Prometheus
    /// output alongside the native `dugite_*` metrics.
    ///
    /// Enables reuse of existing cardano-node Grafana dashboards without
    /// modification.  Disabled by default to avoid polluting the metrics
    /// namespace for operators who do not need it.
    #[arg(long)]
    compat_metrics: bool,

    /// Liveness threshold in seconds for the `/live` HTTP endpoint.
    ///
    /// `/live` returns 503 when no block has been applied within this window
    /// (intended for Kubernetes liveness probes — pod gets restarted when
    /// wedged).  Default 600s.  Set to 0 to disable (always 200).
    #[arg(long, default_value = "600")]
    liveness_threshold_secs: u64,

    /// Maximum number of transactions in the mempool
    #[arg(long, default_value = "16384")]
    mempool_max_tx: usize,

    /// Maximum mempool size in bytes
    #[arg(long, default_value = "536870912")]
    mempool_max_bytes: usize,

    /// Maximum number of ledger snapshots to retain on disk
    #[arg(long, default_value = "2")]
    snapshot_max_retained: usize,

    /// Minimum blocks between bulk-sync snapshots
    #[arg(long, default_value = "50000")]
    snapshot_bulk_min_blocks: u64,

    /// Minimum seconds between bulk-sync snapshots
    #[arg(long, default_value = "360")]
    snapshot_bulk_min_secs: u64,

    /// Storage profile: ultra-memory (32GB), high-memory (16GB, default), low-memory (8GB), or minimal (4GB)
    #[arg(long, default_value = "high-memory")]
    storage_profile: String,

    /// Override: block index type (in-memory or mmap)
    #[arg(long)]
    immutable_index_type: Option<String>,

    /// Override: UTxO backend (in-memory or lsm)
    #[arg(long)]
    utxo_backend: Option<String>,

    /// Override: LSM memtable size in MB
    #[arg(long)]
    utxo_memtable_size_mb: Option<u64>,

    /// Override: LSM block cache size in MB
    #[arg(long)]
    utxo_block_cache_size_mb: Option<u64>,

    /// Override: LSM bloom filter bits per key
    #[arg(long)]
    utxo_bloom_filter_bits: Option<u32>,

    /// Consensus mode override: `praos` or `genesis`.
    ///
    /// When omitted, the value is read from the JSON config field
    /// `ConsensusMode` (default `PraosMode`).  When provided, this CLI flag
    /// wins.  See #535.
    #[arg(long, value_parser = ["praos", "genesis"])]
    consensus_mode: Option<String>,

    /// Force full Phase-2 Plutus validation on all blocks, even during initial sync.
    /// Normally only blocks at tip are fully validated; this enables paranoid/auditing mode.
    #[arg(long)]
    validate_all_blocks: bool,

    /// Issue #655 P2.b — skip apply-time `validate_header_full` for
    /// headers that already passed eager per-peer validation against the
    /// same ledger view's epoch. Default OFF; operators turn it on only
    /// after Phase 1 has been soaked for 7+ days on preview AND preprod
    /// with no unexpected disconnect storms (the original #655
    /// acceptance criteria).
    ///
    /// SAFETY: enabling this skips the apply-time re-check that's been
    /// the source-of-truth for header validity since v1.0. The eager
    /// pass already covered the same crypto against the same snapshot
    /// pointer, but any bug in the eager path becomes silently
    /// load-bearing. Leave OFF until soak passes.
    #[arg(long, default_value = "false")]
    skip_eagerly_validated_header_crypto: bool,

    /// Path to the Dijkstra-era genesis JSON file.
    ///
    /// Overrides the JSON config field `DijkstraGenesisFile`. The file is
    /// parsed at startup but not yet applied to runtime protocol parameters
    /// (issue #462 Phase 6 — parse only; Phase 4 wires pparams 34-37).
    /// Mirrors cardano-node's `--dijkstra-genesis` flag.
    #[arg(long)]
    dijkstra_genesis: Option<PathBuf>,

    // Block producer options (optional — enables block production mode)
    /// Path to the KES signing key file
    #[arg(long)]
    shelley_kes_key: Option<PathBuf>,

    /// Path to the VRF signing key file
    #[arg(long)]
    shelley_vrf_key: Option<PathBuf>,

    /// Path to the operational certificate file
    #[arg(long)]
    shelley_operational_certificate: Option<PathBuf>,

    /// Path to the cold signing key file (for pool ID derivation)
    #[arg(long)]
    shelley_cold_key: Option<PathBuf>,

    #[command(flatten)]
    log: LogArgs,
}

#[derive(clap::Args, Debug)]
struct MithrilImportArgs {
    /// Network magic value (764824073=mainnet, 2=preview, 1=preprod)
    #[arg(long, default_value = "764824073")]
    network_magic: u64,

    /// Path to the database directory
    #[arg(long, default_value = "db")]
    database_path: PathBuf,

    /// Temporary directory for download and extraction
    #[arg(long)]
    temp_dir: Option<PathBuf>,

    /// Override the Mithril genesis verification key (for private networks).
    /// The key must be a JSON hex-encoded Ed25519 verification key string.
    #[arg(long)]
    mithril_genesis_vkey: Option<String>,

    /// Skip Mithril STM certificate chain verification (UNSAFE — for testing only).
    /// When set, the snapshot digest is trusted from the aggregator without
    /// cryptographic proof of the certificate chain.
    #[arg(long)]
    skip_certificate_verification: bool,

    /// Continue the import even if the ancillary archive (Haskell ledger state)
    /// cannot be downloaded.  Without ancillary, the imported ledger state
    /// falls back to genesis-default protocol parameters at the imported tip
    /// (issue #335).  NOT recommended for production.
    #[arg(long)]
    allow_stale_pparams: bool,

    /// Download and import the Mithril ancillary archive (Haskell ledger
    /// state at the immutable tip). When enabled (default), the imported
    /// state replaces the chain-from-genesis replay — bootstrap time
    /// drops from multi-hour to ~15 minutes.
    ///
    /// Pass `--no-include-ancillary` to skip the ancillary download and
    /// rely solely on chunk-by-chunk block replay. This is the
    /// pre-#670 behaviour and is intended for operators who want
    /// dugite to derive the ledger state entirely from its own
    /// validator rather than trusting the Mithril-certified Haskell
    /// snapshot. Trust model and operator-exposure decision are
    /// documented in `docs/src/running/mithril-ancillary.md`.
    ///
    /// Default: `true`. The flag is named so `--include-ancillary` and
    /// `--no-include-ancillary` both work via clap's standard negation.
    #[arg(
        long,
        default_value_t = true,
        action = clap::ArgAction::Set,
        num_args = 0..=1,
        default_missing_value = "true",
        require_equals = false,
    )]
    include_ancillary: bool,

    /// Skip the ancillary download even when `--include-ancillary` would
    /// otherwise enable it. Equivalent to `--include-ancillary=false`.
    /// Provided as a convenience alias because the negated form reads
    /// more naturally in scripts.
    #[arg(long, conflicts_with = "include_ancillary")]
    no_include_ancillary: bool,

    #[command(flatten)]
    log: LogArgs,
}

#[derive(clap::Args, Debug)]
struct DumpSnapshotArgs {
    /// Path to the node configuration file
    #[arg(long)]
    config: PathBuf,

    /// Path to the database directory (must contain immutable/ chunk files)
    #[arg(long, default_value = "db")]
    database_path: PathBuf,

    /// Stop replaying at this slot (dump state at the epoch boundary at or before this slot).
    /// If omitted, replays the entire chain and dumps at every epoch boundary.
    #[arg(long)]
    stop_slot: Option<u64>,

    /// Output file path for JSON dumps. Each epoch's state is one JSON object per line.
    /// Defaults to stdout if not specified.
    #[arg(long)]
    output: Option<PathBuf>,

    /// Output directory for per-epoch JSON files. If set, writes one {epoch}.json
    /// file per epoch instead of NDJSON to --output/stdout.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    #[command(flatten)]
    log: LogArgs,
}

fn build_logging_opts(log: &LogArgs) -> Result<logging::LoggingOpts> {
    let outputs: Result<Vec<logging::LogOutput>, _> =
        log.log_outputs.iter().map(|s| s.parse()).collect();
    let outputs = outputs.map_err(|e| anyhow::anyhow!(e))?;

    let format: logging::LogFormat = log
        .log_format
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    let rotation: logging::LogRotation = log
        .log_file_rotation
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    let stdout_overflow: logging::LogOverflow = log
        .stdout_overflow
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;

    Ok(logging::LoggingOpts {
        outputs,
        format,
        level: log.log_level.clone().unwrap_or_else(|| "info".to_string()),
        log_dir: log.log_dir.to_string_lossy().into_owned(),
        rotation,
        no_color: log.log_no_color,
        log_retention_days: log.log_retention_days,
        stdout_overflow,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install a panic hook that writes a structured message to stderr *and*
    // emits a tracing ERROR event before the process aborts.
    //
    // The release profile uses `panic = "abort"` which normally kills the
    // process immediately — bypassing any buffered log output — making silent
    // crashes extremely difficult to diagnose. This hook ensures that at
    // minimum the panic location and message are written to stderr, and gives
    // the tracing subscriber a brief window to flush its internal buffer.
    std::panic::set_hook(Box::new(|info| {
        // Always write to stderr directly (bypasses any log buffering).
        eprintln!("PANIC: {info}");

        // Also emit through tracing so the message appears in structured log
        // files / journald / file appenders if they are still live.
        tracing::error!(panic_info = %info, "Node panicked — aborting");

        // Give the subscriber a brief window to flush its internal buffer.
        // We cannot call `shutdown_tracer()` here because the subscriber is not
        // guaranteed to be a TracingSubscriber, and `tracing` itself does not
        // expose a flush primitive. A short sleep is a best-effort approach;
        // the subsequent `panic=abort` will terminate the process regardless.
        std::thread::sleep(std::time::Duration::from_millis(50));
    }));

    let cli = Cli::parse();

    // Extract log args and initialize logging before any work
    let log_args = match &cli.command {
        Command::Run(ref args) => Some(&args.log),
        Command::MithrilImport(ref args) => Some(&args.log),
        Command::DumpSnapshot(ref args) => Some(&args.log),
        Command::Db(_) => None,
        Command::VerifyLedgerSnapshot(ref args) => Some(&args.log),
        Command::SnapshotConvert(ref args) => Some(&args.log),
    };
    let log_handle = if let Some(log_args) = log_args {
        Some(logging::init(&build_logging_opts(log_args)?)?)
    } else {
        None
    };

    match cli.command {
        Command::Run(args) => run_node(*args, log_handle).await,
        Command::MithrilImport(args) => run_mithril_import(args).await,
        Command::DumpSnapshot(args) => run_dump_snapshot(args).await,
        Command::Db(args) => run_db_command(args).await,
        Command::VerifyLedgerSnapshot(args) => run_verify_ledger_snapshot(args).await,
        Command::SnapshotConvert(args) => run_snapshot_convert(args).await,
    }
}

/// Compare two ledger snapshots for byte-exact semantic equality (#670).
async fn run_verify_ledger_snapshot(args: VerifyLedgerSnapshotArgs) -> Result<()> {
    info!(
        left = %args.left.display(),
        right = %args.right.display(),
        "Comparing ledger snapshots"
    );
    if args.verbose {
        verify_snapshot::print_scalar_overview(&args.left, &args.right)?;
    }
    if args.show_pool_diffs > 0 {
        verify_snapshot::print_first_pool_param_diffs(
            &args.left,
            &args.right,
            args.show_pool_diffs,
        )?;
    }
    let report = verify_snapshot::verify_snapshots(&args.left, &args.right)?;
    let n = report.print();
    if n > 0 {
        anyhow::bail!("{n} field(s) differ — see report above");
    }
    Ok(())
}

/// Convert a ledger snapshot between the in-memory and LSM UTxO backends
/// without a chain replay (mirror of cardano-node's `snapshot-converter`).
async fn run_snapshot_convert(args: SnapshotConvertArgs) -> Result<()> {
    use dugite_ledger::SnapshotBackend;
    use dugite_node::snapshot_convert::convert_snapshot;

    let target_backend = match args.to_backend.as_str() {
        "mem" => SnapshotBackend::DugiteMem,
        "lsm" => SnapshotBackend::DugiteLsm,
        other => anyhow::bail!("unknown --to-backend `{other}` (expected mem|lsm)"),
    };

    info!(
        source_db = %args.source_db.display(),
        target_db = %args.target_db.display(),
        to_backend = %args.to_backend,
        "snapshot-convert"
    );

    // The conversion is CPU + disk bound (a full UTxO-set scan) — run it off
    // the async runtime so it doesn't stall the reactor.
    let source_db = args.source_db.clone();
    let target_db = args.target_db.clone();
    let stats = tokio::task::spawn_blocking(move || {
        convert_snapshot(&source_db, &target_db, target_backend)
    })
    .await
    .map_err(|e| anyhow::anyhow!("conversion task panicked: {e}"))??;

    info!(
        source_backend = stats.source_backend.as_tag(),
        target_backend = stats.target_backend.as_tag(),
        utxo_count = stats.utxo_count,
        slot = stats.slot,
        "snapshot-convert: done — converted {} UTxOs at slot {} ({} → {})",
        stats.utxo_count,
        stats.slot,
        stats.source_backend.as_tag(),
        stats.target_backend.as_tag(),
    );
    Ok(())
}

/// Replay blocks from ImmutableDB and dump ledger state at epoch boundaries.
///
/// Produces JSON output compatible with cardano-streamer's `dump-snapshot`
/// format for cross-validation of epoch fees, reserves, treasury, and
/// stake distribution.
async fn run_dump_snapshot(args: DumpSnapshotArgs) -> Result<()> {
    use std::io::Write;

    info!(
        config = %args.config.display(),
        database_path = %args.database_path.display(),
        stop_slot = ?args.stop_slot,
        "dump-snapshot: starting epoch-by-epoch ledger state dump"
    );

    // Load node config
    let node_config = config::NodeConfig::load(&args.config)?;
    let config_dir = args
        .config
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();
    node_config.validate(&config_dir)?;

    // Load genesis files and build protocol parameters (same as Node::new)
    let mut protocol_params =
        dugite_primitives::protocol_params::ProtocolParameters::mainnet_defaults();

    let mut byron_epoch_length: u64 = 0;
    let mut byron_slot_duration_ms: u64 = 20_000; // default 20s; overridden by Byron genesis
                                                  // Genesis UTxO entries (address bytes + lovelace), mirroring the running-node path.
                                                  // These are needed so that unredeemed AVVM UTxOs remain in the UTxO set through
                                                  // Byron replay and are correctly purged by `returnRedeemAddrsToReserves` at the
                                                  // Shelley→Allegra boundary (epoch 236 on mainnet, ~299M ADA returned to reserves).
    let mut byron_genesis_utxos: Vec<(Vec<u8>, u64)> = Vec::new();
    // Byron's on-chain protocol parameters, DERIVED from genesis rather than
    // pinned. `eras/byron.rs`'s `ByronFeePolicy` still hardcodes mainnet's
    // `a = 155381` / `b = 21973/500` on the VALIDATION path; these are the same
    // values read from the file, emitted in the dump so the comparison against
    // cardano-streamer's `byronProtocolParams` proves they agree BEFORE the
    // consensus path is switched over to them. Proving first, then swapping, is
    // the order that avoids putting an unverified value on a consensus path.
    let mut byron_pparams: Option<serde_json::Value> = None;
    if let Some(ref genesis_path) = node_config.byron_genesis_file {
        let genesis_path = config_dir.join(genesis_path);
        if let Ok((genesis, _hash)) = genesis::ByronGenesis::load_with_hash(&genesis_path) {
            let k = genesis.security_param();
            byron_epoch_length = 10 * k;
            byron_slot_duration_ms = genesis.slot_duration_ms();
            let bvd = &genesis.block_version_data;
            byron_pparams = bvd.tx_fee_policy.to_exact().map(|(summand, (num, den))| {
                serde_json::json!({
                    "scriptVersion": bvd.script_version,
                    "maxBlockSize": bvd.max_block_size.parse::<u64>().unwrap_or_default(),
                    "maxTxSize": bvd.max_tx_size.parse::<u64>().unwrap_or_default(),
                    "txFeePolicy": {
                        "summand": summand,
                        "multiplier": { "numerator": num, "denominator": den },
                    },
                })
            });
            let utxos = genesis.initial_utxos();
            let total: u64 = utxos.iter().map(|e| e.lovelace).sum();
            info!(
                k,
                epoch_len = byron_epoch_length,
                slot_duration_ms = byron_slot_duration_ms,
                avvm_count = utxos.len(),
                initial_funds = total,
                "Byron genesis loaded"
            );
            byron_genesis_utxos = utxos.into_iter().map(|e| (e.address, e.lovelace)).collect();
        }
    }

    let mut shelley_genesis_opt: Option<genesis::ShelleyGenesis> = None;
    let mut shelley_genesis_hash: Option<dugite_primitives::hash::Hash32> = None;
    if let Some(ref genesis_path) = node_config.shelley_genesis_file {
        let genesis_path = config_dir.join(genesis_path);
        if let Ok((genesis, hash)) = genesis::ShelleyGenesis::load_with_hash(&genesis_path) {
            genesis.apply_to_protocol_params(&mut protocol_params);
            info!(epoch_len = genesis.epoch_length, "Shelley genesis loaded");
            shelley_genesis_hash = Some(hash);
            shelley_genesis_opt = Some(genesis);
        }
    }

    if let Some(ref genesis_path) = node_config.alonzo_genesis_file {
        let genesis_path = config_dir.join(genesis_path);
        if let Ok(genesis) = genesis::AlonzoGenesis::load(&genesis_path) {
            genesis.apply_to_protocol_params(&mut protocol_params);
            info!("Alonzo genesis loaded");
        }
    }

    let mut conway_committee_threshold: Option<(u64, u64)> = None;
    let mut conway_committee_members: Vec<([u8; 32], u64)> = Vec::new();
    let mut conway_constitution: Option<dugite_primitives::transaction::Constitution> = None;
    let mut conway_initial_dreps: Vec<(dugite_primitives::hash::Hash28, u64)> = Vec::new();
    let mut conway_v3_cost_model: Option<Vec<i64>> = None;
    if let Some(ref genesis_path) = node_config.conway_genesis_file {
        let genesis_path = config_dir.join(genesis_path);
        if let Ok(genesis) = genesis::ConwayGenesis::load(&genesis_path) {
            genesis.apply_to_protocol_params(&mut protocol_params);
            conway_committee_threshold = genesis.committee_threshold();
            conway_committee_members = genesis.committee_members();
            conway_constitution = genesis.to_ledger_constitution();
            conway_initial_dreps = genesis.initial_dreps_as_entries();
            conway_v3_cost_model = genesis.plutus_v3_cost_model.clone();
            info!("Conway genesis loaded");
        }
    }

    // Build ConwayGenesisInit for era-transition rules before variables are consumed.
    let conway_genesis_init = if conway_committee_threshold.is_some()
        || !conway_committee_members.is_empty()
        || !conway_initial_dreps.is_empty()
        || conway_v3_cost_model.is_some()
    {
        Some(dugite_ledger::eras::ConwayGenesisInit {
            initial_dreps: conway_initial_dreps.clone(),
            committee_members: conway_committee_members.clone(),
            committee_threshold: conway_committee_threshold,
            constitution: conway_constitution.clone(),
            plutus_v3_cost_model: conway_v3_cost_model.clone(),
        })
    } else {
        None
    };

    // Initialize fresh ledger state from genesis params
    let mut ledger = dugite_ledger::LedgerState::new(protocol_params);
    if let Some(ref sg) = shelley_genesis_opt {
        // Must run BEFORE any seed_genesis_utxos call below so reserves
        // init from the genesis cap (devnets may use 60B, mainnet/preview/preprod 45B).
        ledger.set_max_lovelace_supply(sg.max_lovelace_supply);
    }

    // Seed Conway genesis committee members and threshold (required for governance
    // ratification — without these, check_cc_approval returns false and no
    // proposals requiring CC approval can ratify).
    if let Some((num, den)) = conway_committee_threshold {
        use dugite_primitives::transaction::Rational;
        std::sync::Arc::make_mut(&mut ledger.gov.governance).committee_threshold = Some(Rational {
            numerator: num,
            denominator: den,
        });
    }
    if !conway_committee_members.is_empty() {
        use dugite_primitives::hash::Hash32;
        for (hash_bytes, expiration) in &conway_committee_members {
            let cold_key = Hash32::from_bytes(*hash_bytes);
            std::sync::Arc::make_mut(&mut ledger.gov.governance)
                .committee_expiration
                .insert(cold_key, dugite_primitives::EpochNo(*expiration));
            // Genesis encodes credential type in byte 28 (0x01 = script).
            // Seed script_committee_credentials so the N2C committee-state
            // query reports the correct cold_credential_type for genesis
            // members (without this, all genesis script members are reported
            // as KeyHash).
            if hash_bytes[28] == 0x01 {
                std::sync::Arc::make_mut(&mut ledger.gov.governance)
                    .script_committee_credentials
                    .insert(cold_key);
            }
        }
    }

    // Seed constitution from Conway genesis (CIP-1694 proposal guardrail).
    // Without this, any NewConstitution proposal sees `None` on-chain and
    // UpdateConstitution proposals that reference a prior guardrail script
    // cannot validate on a fresh node.
    if let Some(constitution) = conway_constitution {
        std::sync::Arc::make_mut(&mut ledger.gov.governance).constitution = Some(constitution);
        info!("Conway genesis constitution seeded");
    }

    // Seed initial DReps from Conway genesis. Haskell's `addDefaultDRepsToState`
    // sets expiry = 0 + drep_activity (bootstrap phase, no dormant subtraction).
    if !conway_initial_dreps.is_empty() {
        use dugite_ledger::state::DRepRegistration;
        use dugite_primitives::credentials::Credential;
        use dugite_primitives::value::Lovelace;
        use dugite_primitives::EpochNo;
        let count = conway_initial_dreps.len();
        let drep_activity = ledger.epochs.protocol_params.drep_activity;
        let gov = std::sync::Arc::make_mut(&mut ledger.gov.governance);
        for (hash28, deposit) in conway_initial_dreps {
            let credential = Credential::VerificationKey(hash28);
            let cred_hash = credential.to_typed_hash32();
            gov.dreps.insert(
                cred_hash,
                DRepRegistration {
                    credential,
                    deposit: Lovelace(deposit),
                    anchor: None,
                    registered_epoch: EpochNo(0),
                    drep_expiry: EpochNo(drep_activity),
                    active: true,
                    // A genesis DRep starts with no delegators, like
                    // `ConwayRegDRep`'s `drepDelegs = mempty`.
                    delegs: Default::default(),
                },
            );
        }
        info!(count, "Seeded initial DReps from Conway genesis");
    }

    // Store Conway genesis init data on ledger for era-transition rules.
    ledger.conway_genesis_init = conway_genesis_init;

    // Apply Shelley genesis configuration (epoch length, reserves).
    // Must use set_epoch_length() (not direct field assignment) to compute the
    // correct stability windows (3k/f for Alonzo/Babbage, 4k/f for Conway+)
    // from the network's security parameter k.  With direct assignment the
    // windows default to mainnet values, which are larger than preview's epoch
    // length and cause candidate_nonce to never update.
    // NOTE: set_slot_config() is called AFTER network_magic / shelley_transition_epoch
    // are known (see below), so that the Shelley-anchored SlotConfig can be derived
    // correctly (zero_slot = transition_epoch * byron_epoch_size, not 0).
    if let Some(ref sg) = shelley_genesis_opt {
        ledger.set_epoch_length(sg.epoch_length, sg.security_param);
        ledger.update_quorum = sg.update_quorum;
        // Seed Byron genesis UTxOs via seed_genesis_utxos (which deducts from reserves).
        // reserves = maxLovelaceSupply - Σ(genesis UTxO lovelace)
        //
        // This mirrors the running-node path (Node::init_ledger_state) and is required
        // for `returnRedeemAddrsToReserves` at the Shelley→Allegra boundary to work:
        // unredeemed AVVM UTxOs must be in the live UTxO set so the scan finds them.
        // set_max_lovelace_supply() (called above) already reset reserves to max;
        // seed_genesis_utxos() subtracts the total from reserves.
        if !byron_genesis_utxos.is_empty() {
            ledger.seed_genesis_utxos(&byron_genesis_utxos);
            info!(
                count = byron_genesis_utxos.len(),
                reserves = ledger.epochs.reserves.0,
                "Byron genesis UTxOs seeded (AVVM + nonAvvm); reserves reduced"
            );
        }
    }

    // Seed the nonce state machine from the Shelley genesis hash (matching
    // the running node path in Node::init_ledger_state). Without this,
    // evolving/candidate/epoch nonces all start as ZERO and the entire
    // nonce evolution chain diverges from the Haskell reference.
    if let Some(hash) = shelley_genesis_hash {
        ledger.set_genesis_hash(hash);
    }

    // Set the Shelley transition epoch and Byron epoch length.
    // On preview/preprod (no Byron era), transition = 0 and blocks start
    // directly in Alonzo. On mainnet, transition = 208 (Byron epochs 0-207).
    // The default LedgerState uses mainnet values (208/21600) which would
    // produce incorrect epoch boundaries for other networks.
    // Derive network magic from the Shelley genesis (most reliable source),
    // falling back to node config.  The cstreamer-compatible config files
    // often lack an explicit networkMagic field, which caused the fallback
    // to return mainnet magic (764824073) and completely wrong epoch offsets.
    let network_magic = shelley_genesis_opt
        .as_ref()
        .map(|sg| sg.network_magic)
        .or(node_config.network_magic)
        .unwrap_or_else(|| node_config.network.magic());
    let shelley_transition_epoch =
        crate::node::epoch::shelley_transition_epoch_for_magic(network_magic);
    ledger.set_shelley_transition(shelley_transition_epoch, byron_epoch_length);
    // Apply Plutus SlotConfig anchored at the Shelley hard-fork boundary.
    // Must happen after shelley_transition_epoch is computed (needs network_magic).
    if let Some(ref sg) = shelley_genesis_opt {
        ledger.set_slot_config(sg.slot_config(
            shelley_transition_epoch,
            byron_epoch_length,
            byron_slot_duration_ms,
        ));
    }
    info!(
        network_magic,
        shelley_transition_epoch, byron_epoch_length, "HFC epoch configuration set"
    );

    let immutable_dir = args.database_path.join("immutable");
    if !immutable_dir.is_dir() {
        anyhow::bail!(
            "No immutable directory found at {}. Run mithril-import first.",
            immutable_dir.display()
        );
    }

    // Open output (file or stdout) for NDJSON mode (used when --output-dir is not set).
    let mut output: Box<dyn Write> = match &args.output {
        Some(path) => Box::new(std::io::BufWriter::new(std::fs::File::create(path)?)),
        None => Box::new(std::io::stdout().lock()),
    };

    // Create the per-epoch output directory if requested.
    if let Some(ref dir) = args.output_dir {
        std::fs::create_dir_all(dir)?;
    }

    // Extract max_lovelace_supply from Shelley genesis for correct totalStake
    // computation (RC2): cstreamer defines totalStake = maxLovelaceSupply - reserves,
    // not the sum of pool stakes from the set snapshot.
    let max_lovelace_supply = shelley_genesis_opt
        .as_ref()
        .map(|sg| sg.max_lovelace_supply)
        .unwrap_or(45_000_000_000_000_000u64);

    let stop_slot = args.stop_slot.unwrap_or(u64::MAX);
    let mut last_epoch = u64::MAX;
    let mut epoch_fees: u64 = 0;
    let mut blocks_applied = 0u64;
    let mut epochs_written = 0u64;
    // Becomes the next dumped epoch's `rupdApplied`. Starts Null so the first
    // epoch dumped reports `null`, which is what the oracle emits for its
    // first epoch (verified: `rupdApplied` is null at 208, and at 209 equals
    // 208's `rupdNext`).
    let mut prev_rupd_next = serde_json::Value::Null;
    let start_time = std::time::Instant::now();

    // Skip the expensive full-UTxO rebuild_stake_distribution at each epoch boundary.
    // During dump-snapshot replay from genesis, every block is applied sequentially
    // with full incremental stake tracking, so the stake_map is always accurate.
    // The full rebuild is only needed after Mithril import (which skips incremental
    // tracking) or snapshot restore.
    // Incremental stake tracking is accurate from genesis — no full UTxO rebuild
    // needed at epoch boundaries. needs_stake_rebuild defaults to false.

    info!("Replaying blocks from ImmutableDB...");

    // Dump-snapshot replay walks from genesis (start_after_slot = 0).
    mithril::replay_from_chunk_files(&immutable_dir, 0, byron_epoch_length, |cbor| {
        let block = dugite_serialization::decode_block_minimal_with_byron_epoch_length(
            cbor,
            byron_epoch_length,
        )
        .map_err(|e| anyhow::anyhow!("Block decode error: {e}"))?;

        let block_slot = block.slot().0;
        if block_slot > stop_slot {
            return Err(anyhow::anyhow!("STOP"));
        }

        // Capture the ledger's accumulated epoch fees BEFORE apply_block, so we
        // can compute the delta (actual fees collected by the ledger, which correctly
        // handles invalid tx collateral fees vs declared fees).
        let fees_before = ledger.utxo.epoch_fees.0;

        if let Err(e) = ledger.apply_block(&block, dugite_ledger::BlockValidationMode::ApplyOnly) {
            if !format!("{e}").contains("Block does not connect") {
                tracing::warn!(slot = block_slot, "Block apply failed: {e}");
            }
            return Ok(());
        }

        blocks_applied += 1;

        let current_epoch = ledger.epoch.0;

        // Dump state at each epoch transition.
        // The epoch transition (NEWEPOCH rule: reward distribution, nonce rotation,
        // snapshot rotation, protocol param updates) fires inside apply_block when
        // processing the first block of the new epoch.  Cstreamer captures state
        // AFTER the transition, so we read from `ledger` (post-apply) and label
        // with the NEW epoch (current_epoch), matching cstreamer's convention.
        //
        // RC3: accumulate epoch_fees AFTER the transition check so the first block
        //      of a new epoch's fees go into the new epoch's bucket, not the old one.
        if last_epoch != u64::MAX && current_epoch > last_epoch {
            let snapshot = build_epoch_snapshot(
                &ledger,
                current_epoch,
                epoch_fees,
                max_lovelace_supply,
                prev_rupd_next.clone(),
                byron_pparams.clone(),
            );
            // Thread this epoch's `rupdNext` forward to become the next
            // epoch's `rupdApplied`, mirroring upstream's `(json, rupdData)`
            // return. Taken from the built snapshot rather than recomputed, so
            // the two can never disagree about what this epoch's value was.
            prev_rupd_next = snapshot
                .get("rupdNext")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            write_epoch_snapshot(&snapshot, current_epoch, &args.output_dir, &mut output)
                .map_err(|e| anyhow::anyhow!("Snapshot write error: {e}"))?;

            epochs_written += 1;
            info!(
                epoch = current_epoch,
                treasury = ledger.epochs.treasury.0,
                reserves = ledger.epochs.reserves.0,
                pools = ledger.certs.pool_params.len(),
                fees = epoch_fees,
                era = %format!("{}", ledger.era),
                "Epoch snapshot dumped"
            );

            epoch_fees = 0;
        }

        // Use the ledger's own fee tracking (which correctly handles invalid tx
        // collateral fees). After the epoch transition, ledger.utxo.epoch_fees is reset
        // and only includes the current block's fees. For inter-epoch blocks, it
        // accumulates the delta since fees_before.
        let ledger_fees_now = ledger.utxo.epoch_fees.0;
        if current_epoch > last_epoch && last_epoch != u64::MAX {
            // Epoch transitioned: fees_before was the OLD epoch's total.
            // The ledger reset epoch_fees and then added this block's fee.
            // epoch_fees was already captured above; now add this block's fees
            // to the NEW epoch bucket.
            epoch_fees = ledger_fees_now;
        } else {
            // Same epoch: add the delta.
            epoch_fees += ledger_fees_now - fees_before;
        }
        last_epoch = current_epoch;
        Ok(())
    })
    .or_else(|e| {
        if format!("{e}").contains("STOP") {
            Ok(0)
        } else {
            Err(e)
        }
    })?;

    // NO final snapshot. This used to dump the IN-PROGRESS epoch under
    // `last_epoch`'s number, which OVERWROTE the boundary snapshot already
    // written for that epoch — so the last file of every run was mid-epoch
    // state wearing a boundary dump's name. (`epochs_written` exceeded the file
    // count by exactly one, which was the only visible symptom.)
    //
    // It corrupted the comparison in three ways at once and every one of them
    // looked like a ledger bug: `deposits.pool`/`stakeKey`/`total` diverged
    // (filed as a defect before the cause was found — the boundary values are
    // byte-exact), `snapshots.mark.blocks` held a whole epoch of block counts
    // instead of the new epoch's first, and the epoch appeared as the one
    // outlier outside a cleanly-explained window.
    //
    // cardano-streamer dumps at boundaries only, so an in-progress snapshot has
    // no counterpart to compare against and no meaning in this comparison. The
    // stop point is already reported in the completion log.
    let _ = epoch_fees;

    let elapsed = start_time.elapsed();
    info!(
        blocks = blocks_applied,
        epochs_written,
        elapsed_secs = elapsed.as_secs(),
        "dump-snapshot complete"
    );

    Ok(())
}

/// Render a credential hash the way cardano-ledger's `credToText` does when a
/// `Credential` is used as a JSON map KEY: `keyHash-<56 hex>` /
/// `scriptHash-<56 hex>` (`Cardano/Ledger/Credential.hs`).
///
/// dugite stores these as `Credential::to_typed_hash32`: bytes `[..28]` are the
/// hash, byte `[28]` is `0x01` for a script and `0x00` for a key. The kind is
/// therefore READ from the key, never assumed — labelling every entry
/// `keyHash-` is precisely the defect that made `drepDistr` mismatch the oracle
/// on chains that have script-credentialled DReps or committee members, and
/// preprod's whole constitutional committee is script-credentialled.
fn credential_key(hash: &dugite_primitives::hash::Hash32) -> String {
    let bytes = hash.as_bytes();
    let kind = if bytes[28] == 0x01 {
        "scriptHash"
    } else {
        "keyHash"
    };
    format!("{kind}-{}", hex::encode(&bytes[..28]))
}

/// The same credential in a VALUE position, where cardano-ledger emits an
/// object rather than the prefixed text form: `{"keyHash": "<56 hex>"}` /
/// `{"scriptHash": "<56 hex>"}` (`Credential.hs`, the `ToJSON` instance as
/// distinct from `ToJSONKey`).
fn credential_value(hash: &dugite_primitives::hash::Hash32) -> serde_json::Value {
    let bytes = hash.as_bytes();
    let kind = if bytes[28] == 0x01 {
        "scriptHash"
    } else {
        "keyHash"
    };
    serde_json::json!({ kind: hex::encode(&bytes[..28]) })
}

/// `GovActionId` — `{"govActionIx": <int>, "txId": "<64 hex>"}`, or `null`.
///
/// NOT `"<txid>#<ix>"`. dugite emitted the string form under a top-level
/// `enactedRoots` key that upstream does not have at all; the object form is
/// what `Conway/Governance/Procedures.hs` prints, confirmed against preprod
/// epoch 179's first non-null root.
fn gov_action_id_json(
    id: Option<&dugite_primitives::transaction::GovActionId>,
) -> serde_json::Value {
    match id {
        None => serde_json::Value::Null,
        Some(id) => serde_json::json!({
            "govActionIx": id.action_index,
            "txId": id.transaction_id.to_hex(),
        }),
    }
}

/// A `BoundedRatio` as cardano-ledger prints it: a JSON NUMBER, not a
/// `{numerator, denominator}` object.
///
/// Upstream goes `Rational -> Scientific -> JSON`, so `3/1000` reaches the file
/// as `0.0030` and `15/1` as `15`. Both parse to the same value as this f64
/// division, which is what the comparison is on — it compares parsed JSON
/// values, and folds an integral float to an int before hashing, so `15` and
/// `15.0` are one value in both the deep-diff and the digest paths.
fn rational_json(r: &dugite_primitives::transaction::Rational) -> serde_json::Value {
    if r.denominator == 0 {
        // Not reachable from a decoded parameter (every `BoundedRatio` has a
        // non-zero denominator) — but a NaN would serialise as `null` and read
        // as an absent field rather than a broken one.
        return serde_json::Value::Null;
    }
    serde_json::json!(r.numerator as f64 / r.denominator as f64)
}

/// `Committee` — `{"members": {<credKey>: <expiryEpoch>}, "threshold": <ratio>}`,
/// or `null` for `SNothing` (no committee, e.g. after an enacted `NoConfidence`).
fn committee_json(
    members: &imbl::HashMap<dugite_primitives::hash::Hash32, dugite_primitives::time::EpochNo>,
    threshold: Option<&dugite_primitives::transaction::Rational>,
) -> serde_json::Value {
    let Some(threshold) = threshold else {
        return serde_json::Value::Null;
    };
    let mut m = serde_json::Map::new();
    for (cold, expiry) in members {
        m.insert(
            credential_key(cold),
            serde_json::Value::Number(expiry.0.into()),
        );
    }
    serde_json::json!({
        "members": serde_json::Value::Object(m),
        // `threshold` is a `UnitInterval` and prints as the numerator/
        // denominator OBJECT here, unlike the pparams thresholds which print as
        // decimals — confirmed against preprod, which carries 2/3.
        "threshold": {
            "numerator": threshold.numerator,
            "denominator": threshold.denominator,
        },
    })
}

/// `Constitution` — `{"anchor": {"dataHash", "url"}}` plus `"script"` ONLY when
/// the guardrail script is present.
///
/// The key is ABSENT, not null, when there is no script: upstream builds the
/// pair list with a comprehension guard
/// (`["script" .= s | SJust s <- [constitutionScript]]`,
/// `Conway/Governance/Procedures.hs`). Emitting `"script": null` instead would
/// be a schema gap on one side in every epoch of a chain without a guardrail.
fn constitution_json(
    c: Option<&dugite_primitives::transaction::Constitution>,
) -> serde_json::Value {
    let Some(c) = c else {
        return serde_json::Value::Null;
    };
    let mut o = serde_json::Map::new();
    o.insert(
        "anchor".to_string(),
        serde_json::json!({
            "dataHash": c.anchor.data_hash.to_hex(),
            "url": c.anchor.url,
        }),
    );
    if let Some(script) = &c.script_hash {
        o.insert(
            "script".to_string(),
            serde_json::Value::String(script.to_hex()),
        );
    }
    serde_json::Value::Object(o)
}

/// `CommitteeState` — `{"csCommitteeCreds": {<coldKey>: <authorization>}}`.
///
/// The authorization is a TAGGED SUM, not a bare hash:
///
/// ```text
/// {"tag": "CommitteeHotCredential", "contents": {"scriptHash"|"keyHash": "<56 hex>"}}
/// {"tag": "CommitteeMemberResigned", "contents": null | {"url", "dataHash"}}
/// ```
///
/// Only members that have authorized a hot key or resigned appear — a seated
/// member who has done neither has no entry, which is why preprod's map is
/// empty for its first eight Conway epochs while the committee already has
/// seven members.
///
/// Resignation wins over any hot-key entry. It is permanent upstream
/// (`checkAndOverwriteCommitteeMemberState`) and dugite drops the hot key on
/// resign, so the two maps are disjoint in practice; the precedence makes that
/// independent of the drop rather than reliant on it.
fn committee_state_json(gov: &dugite_ledger::state::GovernanceState) -> serde_json::Value {
    let mut creds = serde_json::Map::new();
    for (cold, hot) in &gov.committee_hot_keys {
        creds.insert(
            credential_key(cold),
            serde_json::json!({
                "tag": "CommitteeHotCredential",
                "contents": credential_value(hot),
            }),
        );
    }
    for (cold, anchor) in &gov.committee_resigned {
        creds.insert(
            credential_key(cold),
            serde_json::json!({
                "tag": "CommitteeMemberResigned",
                "contents": anchor.as_ref().map(|a| serde_json::json!({
                    "url": a.url,
                    "dataHash": a.data_hash.to_hex(),
                })).unwrap_or(serde_json::Value::Null),
            }),
        );
    }
    serde_json::json!({ "csCommitteeCreds": serde_json::Value::Object(creds) })
}

/// Conway `PParams` in the shape cardano-ledger's `ToJSON` prints — the 31-key
/// cardano-cli-style record, NOT the small rational-rendered summary this dump
/// emits at top level under `protocolParams`.
///
/// The two are different records and must not be conflated: the top-level one
/// carries `a0`/`d`/`rho`/`tau` as `{numerator, denominator}` objects and holds
/// the PREVIOUS epoch's parameters, while this one names every Conway parameter
/// the way cardano-cli does and renders every ratio as a decimal.
fn conway_pparams_json(
    pp: &dugite_primitives::protocol_params::ProtocolParameters,
) -> serde_json::Value {
    let mut cost_models = serde_json::Map::new();
    for (name, model) in [
        ("PlutusV1", &pp.cost_models.plutus_v1),
        ("PlutusV2", &pp.cost_models.plutus_v2),
        ("PlutusV3", &pp.cost_models.plutus_v3),
        ("PlutusV4", &pp.cost_models.plutus_v4),
    ] {
        if let Some(m) = model {
            cost_models.insert(name.to_string(), serde_json::json!(m));
        }
    }
    // `unknown_cost_models` (language keys >= 4) is deliberately NOT emitted:
    // no such key has ever been on-chain, so the oracle's rendering for it has
    // never been observed and would have to be guessed. If one ever appears,
    // this omission shows up as a schema gap — which is the honest signal —
    // rather than as a confidently wrong key name.

    serde_json::json!({
        "collateralPercentage": pp.collateral_percentage,
        "committeeMaxTermLength": pp.committee_max_term_length,
        "committeeMinSize": pp.committee_min_size,
        "costModels": serde_json::Value::Object(cost_models),
        "dRepActivity": pp.drep_activity,
        "dRepDeposit": pp.drep_deposit.0,
        "dRepVotingThresholds": {
            "committeeNoConfidence": rational_json(&pp.dvt_committee_no_confidence),
            "committeeNormal": rational_json(&pp.dvt_committee_normal),
            "hardForkInitiation": rational_json(&pp.dvt_hard_fork),
            "motionNoConfidence": rational_json(&pp.dvt_no_confidence),
            "ppEconomicGroup": rational_json(&pp.dvt_pp_economic_group),
            "ppGovGroup": rational_json(&pp.dvt_pp_gov_group),
            "ppNetworkGroup": rational_json(&pp.dvt_pp_network_group),
            "ppTechnicalGroup": rational_json(&pp.dvt_pp_technical_group),
            "treasuryWithdrawal": rational_json(&pp.dvt_treasury_withdrawal),
            "updateToConstitution": rational_json(&pp.dvt_constitution),
        },
        "executionUnitPrices": {
            "priceMemory": rational_json(&pp.execution_costs.mem_price),
            "priceSteps": rational_json(&pp.execution_costs.step_price),
        },
        "govActionDeposit": pp.gov_action_deposit.0,
        "govActionLifetime": pp.gov_action_lifetime,
        "maxBlockBodySize": pp.max_block_body_size,
        "maxBlockExecutionUnits": {
            "memory": pp.max_block_ex_units.mem,
            "steps": pp.max_block_ex_units.steps,
        },
        "maxBlockHeaderSize": pp.max_block_header_size,
        "maxCollateralInputs": pp.max_collateral_inputs,
        "maxTxExecutionUnits": {
            "memory": pp.max_tx_ex_units.mem,
            "steps": pp.max_tx_ex_units.steps,
        },
        "maxTxSize": pp.max_tx_size,
        "maxValueSize": pp.max_val_size,
        "minFeeRefScriptCostPerByte": rational_json(&pp.min_fee_ref_script_cost_per_byte),
        "minPoolCost": pp.min_pool_cost.0,
        "monetaryExpansion": rational_json(&pp.rho),
        "poolPledgeInfluence": rational_json(&pp.a0),
        "poolRetireMaxEpoch": pp.e_max,
        "poolVotingThresholds": {
            "committeeNoConfidence": rational_json(&pp.pvt_committee_no_confidence),
            "committeeNormal": rational_json(&pp.pvt_committee_normal),
            "hardForkInitiation": rational_json(&pp.pvt_hard_fork),
            "motionNoConfidence": rational_json(&pp.pvt_motion_no_confidence),
            "ppSecurityGroup": rational_json(&pp.pvt_pp_security_group),
        },
        "protocolVersion": {
            "major": pp.protocol_version_major,
            "minor": pp.protocol_version_minor,
        },
        "stakeAddressDeposit": pp.key_deposit.0,
        "stakePoolDeposit": pp.pool_deposit.0,
        "stakePoolTargetNum": pp.n_opt,
        "treasuryCut": rational_json(&pp.tau),
        "txFeeFixed": pp.min_fee_b,
        "txFeePerByte": pp.min_fee_a,
        "utxoCostPerByte": pp.ada_per_utxo_byte.0,
    })
}

/// Greatest common divisor (Euclidean algorithm).
fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = b;
        b = a % b;
        a = t;
    }
    a
}

/// Serialise one mark/set/go `StakeSnapshot` into the cstreamer JSON format.
///
/// Cstreamer includes full delegation maps, pool parameters, individual stake, and
/// per-pool block counts so that the cross-validation script can catch divergences in
/// snapshot rotation, staking, and pool parameter tracking.
/// Reduce a flat `key -> scalar` JSON map to `{count, sum, digest}`.
///
/// The canonical form is deliberately NOT JSON: entries are sorted by key and
/// joined as `key:value;`, with the value rendered as a bare integer or bare
/// string. JSON would drag in key-ordering, float formatting and escaping —
/// three ways for two implementations to disagree about identical data. Both
/// maps this is applied to are hex-keyed with integer or hex-string values, so
/// this form is unambiguous and trivially reproducible.
///
/// `sum` is emitted only when every value is an integer; for `delegations`
/// (values are pool-id strings) it is absent rather than zero, because a zero
/// there would read as a real total.
///
/// `scripts/validation/diff-cstreamer-dumps.py` reproduces this byte for byte
/// and `digest_of_map_matches_the_comparator` pins the two together.
fn digest_of_map(m: &serde_json::Map<String, serde_json::Value>) -> serde_json::Value {
    use sha2::{Digest, Sha256};

    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort_unstable();

    let mut hasher = Sha256::new();
    let mut sum: Option<u128> = Some(0);
    for k in &keys {
        let v = &m[*k];
        let rendered = match v {
            serde_json::Value::Number(n) => {
                if let (Some(acc), Some(u)) = (sum, n.as_u64()) {
                    sum = acc.checked_add(u as u128);
                } else {
                    sum = None;
                }
                n.to_string()
            }
            serde_json::Value::String(s) => {
                sum = None;
                s.clone()
            }
            other => {
                sum = None;
                other.to_string()
            }
        };
        hasher.update(k.as_bytes());
        hasher.update(b":");
        hasher.update(rendered.as_bytes());
        hasher.update(b";");
    }

    let mut out = serde_json::Map::new();
    out.insert(
        "__count__".into(),
        serde_json::Value::Number(m.len().into()),
    );
    if let Some(s) = sum {
        if let Ok(v) = u64::try_from(s) {
            out.insert("__sum__".into(), serde_json::Value::Number(v.into()));
        }
    }
    out.insert(
        "__digest__".into(),
        serde_json::Value::String(hex::encode(hasher.finalize())),
    );
    serde_json::Value::Object(out)
}

fn serialize_stake_snapshot(
    name: &str,
    snapshot: &dugite_ledger::state::StakeSnapshot,
    override_blocks: Option<&std::collections::HashMap<dugite_primitives::hash::Hash28, u64>>,
) -> serde_json::Value {
    use dugite_primitives::transaction::Relay;

    // delegations: credential hash → pool ID
    // Key format: "{type}Hash-{56_hex_chars}" where type is determined by
    // byte 28 of the Hash32 (0x00=key, 0x01=script), matching cstreamer.
    let delegations: serde_json::Map<String, serde_json::Value> = snapshot
        .delegations
        .iter()
        .map(|(cred, pool_id)| {
            let prefix = if cred.as_bytes()[28] == 0x01 {
                "scriptHash"
            } else {
                "keyHash"
            };
            let key = format!("{}-{}", prefix, hex::encode(&cred.as_bytes()[..28]));
            let val = serde_json::Value::String(hex::encode(pool_id.as_bytes()));
            (key, val)
        })
        .collect();

    // poolParams: pool_id hex → rich pool params object
    let pool_params: serde_json::Map<String, serde_json::Value> = snapshot
        .pool_params
        .iter()
        .map(|(pool_id, reg)| {
            let key = hex::encode(pool_id.as_bytes());

            // Owners: list of 28-byte key hash hex strings
            let owners: Vec<serde_json::Value> = reg
                .owners
                .iter()
                .map(|o| serde_json::Value::String(hex::encode(o.as_bytes())))
                .collect();

            // margin as f64 ratio
            let margin = if reg.margin_denominator == 0 {
                0.0f64
            } else {
                reg.margin_numerator as f64 / reg.margin_denominator as f64
            };

            // rewardAccount: decode the raw bytes (byte 0 = header, bytes 1..29 = cred hash)
            let reward_account_json = if reg.reward_account.len() >= 29 {
                let header = reg.reward_account[0];
                let network = if header & 0x0F == 1 {
                    "Mainnet"
                } else {
                    "Testnet"
                };
                let cred_hex = hex::encode(&reg.reward_account[1..29]);
                // Bit 4 of the header selects the credential KIND — reward
                // address headers are 0xe0/0xe1 for a key hash and 0xf0/0xf1
                // for a script hash. This read is the ledger's own
                // (`eras/common.rs::reward_account_to_hash`), which has always
                // had it right; only this serialiser hardcoded `keyHash`.
                //
                // Every script-credentialled pool reward account was therefore
                // mislabelled. Measured on preprod, where it made three
                // `snapshots.{mark,set,go}.poolParams.<pool>.rewardAccount
                // .credential.*` paths a SCHEMA GAP in ~90 epochs each: dugite
                // emitted a `keyHash` key the oracle does not have and lacked
                // the `scriptHash` key it does. Same family as the `drepDistr`
                // key defect — a credential kind assumed rather than read.
                let kind = if header & 0x10 != 0 {
                    "scriptHash"
                } else {
                    "keyHash"
                };
                serde_json::json!({
                    "credential": { kind: cred_hex },
                    "network": network,
                })
            } else {
                serde_json::Value::Null
            };

            // relays
            let relays: Vec<serde_json::Value> = reg
                .relays
                .iter()
                .map(|r| match r {
                    Relay::SingleHostAddr { port, ipv4, ipv6 } => serde_json::json!({
                        "type": "SingleHostAddr",
                        "port": port,
                        "ipv4": ipv4.map(|ip| format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])),
                        "ipv6": ipv6.map(|b| dugite_primitives::transaction::ipv6_from_ledger_bytes(&b).to_string()),
                    }),
                    Relay::SingleHostName { port, dns_name } => serde_json::json!({
                        "type": "SingleHostName",
                        "port": port,
                        "dnsName": dns_name,
                    }),
                    Relay::MultiHostName { dns_name } => serde_json::json!({
                        "type": "MultiHostName",
                        "dnsName": dns_name,
                    }),
                })
                .collect();

            let val = serde_json::json!({
                "publicKey": hex::encode(reg.pool_id.as_bytes()),
                "owners": owners,
                "pledge": reg.pledge.0,
                "cost": reg.cost.0,
                "margin": margin,
                "rewardAccount": reward_account_json,
                "vrf": hex::encode(reg.vrf_keyhash.as_bytes()),
                "relays": relays,
                "metadata": reg.metadata_url.as_ref().map(|url| serde_json::json!({
                    "url": url,
                    "hash": reg.metadata_hash.as_ref().map(|h| hex::encode(h.as_bytes())),
                })),
            });

            (key, val)
        })
        .collect();

    // stake: per-credential lovelace
    // Key format: "{type}Hash-{56_hex_chars}" (byte 28 encodes key vs script).
    let stake: serde_json::Map<String, serde_json::Value> = snapshot
        .stake_distribution
        .iter()
        .map(|(cred, lovelace)| {
            let prefix = if cred.as_bytes()[28] == 0x01 {
                "scriptHash"
            } else {
                "keyHash"
            };
            let key = format!("{}-{}", prefix, hex::encode(&cred.as_bytes()[..28]));
            let val = serde_json::Value::Number(lovelace.0.into());
            (key, val)
        })
        .collect();

    // blocks: per-pool block production count.
    // Cstreamer uses Haskell's nesBcur/nesBprev (tracked separately from snapshots):
    //   - mark.blocks = nesBcur (blocks produced in current epoch so far)
    //   - go.blocks   = nesBprev (blocks from previous epoch)
    //   - set.blocks   = not included (None/omitted)
    // Callers pass the appropriate block source, or None to omit.
    // `DUGITE_DUMP_DIGEST=1` replaces the two credential-scale maps with a
    // digest record. At mainnet epoch 271 they are 98% of the file:
    //
    //   snapshots.*.delegations   3 x ~78 MB   (628,263 entries)
    //   snapshots.*.stake         3 x ~47 MB   (612,174 entries)
    //   snapshots.*.poolParams    3 x  2.1 MB  (2,736 entries — left as-is)
    //   ------------------------------------------------------------------
    //   total                         384 MB  ->  ~8 MB
    //
    // Without this a genesis->tip comparison is 1-2 TB of dumps, which is the
    // blocker on extending past Mary, not sync time. It costs no detection
    // power: the digest covers every entry, and the comparison's own negative
    // test catches a ONE lovelace change to ONE credential in a 37,819-entry
    // map through the digest alone.
    let digest_mode = std::env::var("DUGITE_DUMP_DIGEST").as_deref() == Ok("1");
    let (delegations, stake) = if digest_mode {
        (digest_of_map(&delegations), digest_of_map(&stake))
    } else {
        (
            serde_json::Value::Object(delegations),
            serde_json::Value::Object(stake),
        )
    };

    // No `epoch` key. Upstream's `snapshotInfo` (cardano-streamer Run.hs:296)
    // emits exactly name/stake/delegations/poolParams, plus `blocks` for mark
    // and go — a snapshot does not carry an epoch number there. Emitting one
    // made all three snapshots a SCHEMA GAP in every paired epoch, and the
    // `go` fallback branch below filled it with a hardcoded 0, so the wrong
    // value was never visible to anything.
    let mut result = serde_json::json!({
        "name": name,
        "delegations": delegations,
        "poolParams": pool_params,
        "stake": stake,
    });
    if let Some(block_map) = override_blocks {
        let blocks: serde_json::Map<String, serde_json::Value> = block_map
            .iter()
            .map(|(pool_id, count)| {
                let key = hex::encode(pool_id.as_bytes());
                let val = serde_json::Value::Number((*count).into());
                (key, val)
            })
            .collect();
        result["blocks"] = serde_json::Value::Object(blocks);
    }
    result
}

/// Build the richer epoch-snapshot JSON object from the current ledger state.
///
/// Called at every epoch transition (the ledger already reflects the NEWEPOCH
/// rule — reward distribution, nonce rotation, snapshot rotation, protocol
/// param updates) and for the final in-progress epoch.  Fields match the
/// cstreamer reference format for cross-validation.
fn build_epoch_snapshot(
    ledger: &dugite_ledger::LedgerState,
    epoch: u64,
    _driver_epoch_fees: u64,
    max_lovelace_supply: u64,
    // The PREVIOUS epoch's `rupdNext`, verbatim. Upstream does not recompute
    // an "applied" reward update: `buildSnapshotJson` returns
    // `(json, rupdData)` and the driver threads the prior value straight back
    // in as `mPrevRupd` (cardano-streamer Run.hs:169, 370), so
    // `rupdApplied[E] == rupdNext[E-1]` by construction.
    //
    // Verified against real oracle output rather than read off the source:
    // epoch 209's `rupdApplied` is byte-identical to epoch 208's `rupdNext`,
    // and epoch 208's is `null`.
    //
    // This therefore adds NO independent signal — it re-compares, one epoch
    // later, what `rupdNext` already compared. It is emitted to close a schema
    // gap honestly, not as new coverage of the applied reward update.
    // Computing it from what dugite actually applied would be more
    // informative and would no longer match the oracle, manufacturing
    // divergences that are definitional.
    prev_rupd_next: serde_json::Value,
    // Byron's on-chain protocol parameters, DERIVED from the Byron genesis file.
    // `None` when the config declares no Byron genesis, i.e. a
    // Shelley-from-genesis chain that has no Byron era to describe.
    byron_pparams: Option<serde_json::Value>,
) -> serde_json::Value {
    // `epochFees` must be the LEDGER's `ssFee`, not a fee total the dump driver
    // accumulated for itself.
    //
    // cardano-streamer reports `ssFee` out of the snapshots record
    // (`SnapShots {ssFee = feeCoin} = epochState ^. esSnapshotsL`), which is
    // real ledger state, frozen by SNAP. dugite's driver instead summed
    // per-block fee deltas across the epoch and reset at each boundary — a
    // harness artefact that exists nowhere in the ledger. The two are different
    // quantities, so comparing them measured nothing and reported 62 of 64
    // epochs divergent for it (18,336,558,632 vs 7,666,346,424 at epoch 210).
    //
    // A definitional mismatch and a real divergence look identical from the
    // diff, which is why the oracle's column definitions get checked first.
    let epoch_fees = ledger.epochs.snapshots.ss_fee.0;
    // RC2: totalStake = maxLovelaceSupply - reserves (matches cstreamer).
    let total_stake = max_lovelace_supply.saturating_sub(ledger.epochs.reserves.0);

    // Active stake from the "go" snapshot (used for reward distribution).
    let active_stake: u64 = ledger
        .epochs
        .snapshots
        .go
        .as_ref()
        .map(|s| s.pool_stake.values().map(|v| v.0).sum())
        .unwrap_or(0);

    // Pool distribution from the "set" snapshot with extended cstreamer fields.
    let total_active_stake = ledger
        .epochs
        .snapshots
        .set
        .as_ref()
        .map(|s| s.pool_stake.values().map(|v| v.0).sum::<u64>())
        .unwrap_or(0);

    let pool_distribution: Vec<serde_json::Value> = ledger
        .epochs
        .snapshots
        .set
        .as_ref()
        .map(|s| {
            s.pool_stake
                .iter()
                .map(|(pool_id, stake_lovelace)| {
                    let lv = stake_lovelace.0;
                    let pct = if total_active_stake > 0 {
                        lv as f64 / total_active_stake as f64 * 100.0
                    } else {
                        0.0
                    };
                    // Reduce the stake fraction to simplest form (matching cstreamer).
                    let (num, den) = if lv == 0 {
                        (0, 1) // Zero stake: 0/1
                    } else if total_active_stake > 0 {
                        let g = gcd(lv, total_active_stake);
                        (lv / g, total_active_stake / g)
                    } else {
                        (0, 1)
                    };
                    serde_json::json!({
                        "poolId": hex::encode(pool_id.as_bytes()),
                        "stake": { "numerator": num, "denominator": den },
                        "stakeLovelace": lv,
                        "stakePercent": pct,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    // Deposit accounting.
    let deposit_stake_key = ledger.certs.total_stake_key_deposits;
    let deposit_pool: u64 = ledger.certs.pool_deposits.values().sum();
    let deposit_drep: u64 = ledger
        .gov
        .governance
        .dreps
        .values()
        .map(|r| r.deposit.0)
        .sum();
    let deposit_proposal: u64 = ledger
        .gov
        .governance
        .proposals
        .values()
        .map(|p| p.procedure.deposit.0)
        .sum();
    let deposit_total = deposit_stake_key + deposit_pool + deposit_drep + deposit_proposal;

    // DRep distribution, keyed exactly as cardano-streamer's
    // `Map.map fromCompact (psDRepDistr snap)` renders it:
    //   drep-keyHash-<56 hex> | drep-scriptHash-<56 hex>
    //   drep-alwaysAbstain    | drep-alwaysNoConfidence
    //
    // The cache key is a TYPED Hash32, not a bare hash:
    // `DRep::credential_hash32` writes the 28-byte credential into
    // `bytes[..28]` and the discriminator into `bytes[28]` — 0x00 for a key
    // hash (by zero-padding) and 0x01 for a script hash. So the kind is
    // recoverable here and must be READ rather than assumed.
    //
    // Both halves of the previous key were wrong, and each would have produced
    // a confident false divergence against the oracle:
    //   * `&hash.to_hex()[..30]` truncated to 15 of the 28 bytes;
    //   * every entry was labelled `keyHash`, including script DReps. Real
    //     preprod data has both — epoch 166 carries 9 key-hash DReps and one
    //     script DRep holding more lovelace than all nine combined.
    let (drep_cache, drep_no_conf, drep_abstain_val) = ledger.build_drep_power_cache();
    let mut drep_distr_map = serde_json::Map::new();
    for (hash, power) in &drep_cache {
        let bytes = hash.as_bytes();
        let kind = if bytes[28] == 0x01 {
            "scriptHash"
        } else {
            "keyHash"
        };
        drep_distr_map.insert(
            format!("drep-{kind}-{}", hex::encode(&bytes[..28])),
            serde_json::Value::Number((*power).into()),
        );
    }
    // The two pseudo-DReps are emitted only when they carry stake.
    //
    // `psDRepDistr` is a Map, so upstream simply has NO entry for a pseudo-DRep
    // nobody delegated to — measured on preprod epoch 166, where the oracle
    // emits `drep-alwaysAbstain` and omits `drep-alwaysNoConfidence` entirely.
    // Emitting a zero unconditionally produced exactly one spurious key in
    // every such epoch, and a per-epoch constant difference is the noise that
    // hid a real IPv6 defect in #1078.
    //
    // PRESENCE, NOT AMOUNT. The gate is the pulser's two `*_delegated` flags,
    // which exist for exactly this (#994): upstream's `Map.insertWith` creates
    // the key when an account delegates to a predefined DRep, so a delegated
    // pseudo-DRep holding zero stake IS a key with value 0, while an
    // undelegated one is absent. Gating on `stake > 0` collapses those two and
    // is wrong in the first case; gating on the flag reproduces the key set.
    //
    // SUPERSEDED LIMITATION: `build_drep_power_cache` returns the two as plain
    // counters, so on its own dugite cannot distinguish "absent from the map"
    // from "present with zero" — which is why the flags are read from the
    // pulsing snapshot directly rather than inferred from the amounts.
    //
    // With no pulser there is no key set to reproduce (Haskell's `Default` is
    // `DRComplete def def`, an empty map), so neither is emitted.
    let (no_conf_present, abstain_present) = ledger
        .gov
        .governance
        .pulsing_snapshot()
        .map(|s| (s.drep_no_confidence_delegated, s.drep_abstain_delegated))
        .unwrap_or((false, false));
    if no_conf_present {
        drep_distr_map.insert(
            "drep-alwaysNoConfidence".to_string(),
            serde_json::Value::Number(drep_no_conf.into()),
        );
    }
    if abstain_present {
        drep_distr_map.insert(
            "drep-alwaysAbstain".to_string(),
            serde_json::Value::Number(drep_abstain_val.into()),
        );
    }
    let drep_distr = serde_json::Value::Object(drep_distr_map);

    // `conwayGov` — cardano-streamer's `extractConwayGovData` (Run.hs:150-166),
    // five keys and no others:
    //
    // ```haskell
    // let (snap, ratifyState) = finishDRepPulser (nes ^. newEpochStateDRepPulsingStateL)
    //     drepDistr      = Map.map fromCompact (psDRepDistr snap)
    //     committee      = nes ^. newEpochStateGovStateL . committeeGovStateL
    //     constitution   = nes ^. newEpochStateGovStateL . constitutionGovStateL
    //     committeeState = nes ^. … . certVStateL . vsCommitteeStateL
    //     nextEnactState = ratifyState ^. rsEnactStateL
    // ```
    //
    // Note the two DIFFERENT provenances, which is the whole subtlety of this
    // record: `committee` and `constitution` are read LIVE from the governance
    // state, while `nextEnactState`'s copies of the same two come from the
    // forced pulser and therefore already carry whatever the NEXT boundary will
    // enact. Emitting one value in both places would agree in every epoch that
    // enacts nothing — see `EnactedGovTerms`.
    //
    // `null` before Conway: `applyConwayNewEpochState` returns Nothing for
    // earlier eras, and the comparator's `era_applicable` models exactly that.
    let conway_gov: serde_json::Value = if ledger.era < dugite_primitives::era::Era::Conway {
        serde_json::Value::Null
    } else {
        let gov = &ledger.gov.governance;
        let next = ledger.gov.governance.ratify_plan();
        serde_json::json!({
            "drepDistr": drep_distr,
            "committee": committee_json(&gov.committee_expiration, gov.committee_threshold.as_ref()),
            "constitution": constitution_json(gov.constitution.as_ref()),
            "committeeState": committee_state_json(gov),
            "nextEnactState": next.map(|n| serde_json::json!({
                "committee": committee_json(
                    &n.enact_state.committee_expiration,
                    n.enact_state.committee_threshold.as_ref(),
                ),
                "constitution": constitution_json(n.enact_state.constitution.as_ref()),
                "curPParams": conway_pparams_json(&n.cur_pparams),
                "prevPParams": conway_pparams_json(&ledger.epochs.prev_protocol_params),
                "prevGovActionIds": {
                    "PParamUpdate": gov_action_id_json(n.enact_state.prev_gov_action_ids.pparam.as_ref()),
                    "HardFork": gov_action_id_json(n.enact_state.prev_gov_action_ids.hard_fork.as_ref()),
                    "Committee": gov_action_id_json(n.enact_state.prev_gov_action_ids.committee.as_ref()),
                    "Constitution": gov_action_id_json(n.enact_state.prev_gov_action_ids.constitution.as_ref()),
                },
            })).unwrap_or(serde_json::Value::Null),
        })
    };

    // Protocol params summary.
    // Use prev_protocol_params (esPrevPp) to match cstreamer's convention:
    // cstreamer dumps the params that governed the PREVIOUS epoch, not the
    // post-UPEC params for the current epoch.
    let prev_pp = &ledger.epochs.prev_protocol_params;
    let protocol_params = serde_json::json!({
        "a0": { "numerator": prev_pp.a0.numerator, "denominator": prev_pp.a0.denominator },
        "d":  { "numerator": prev_pp.d.numerator,  "denominator": prev_pp.d.denominator  },
        "rho": { "numerator": prev_pp.rho.numerator, "denominator": prev_pp.rho.denominator },
        "tau": { "numerator": prev_pp.tau.numerator, "denominator": prev_pp.tau.denominator },
        "nOpt": prev_pp.n_opt,
        "minPoolCost": prev_pp.min_pool_cost.0,
        "protocolVersion": {
            "major": prev_pp.protocol_version_major,
            "minor": prev_pp.protocol_version_minor,
        },
    });

    // `rupdNext` — the reward update this epoch will apply at its NEXT
    // boundary, forced to completion exactly as cardano-streamer forces its
    // own pulser before dumping.
    //
    // This read `ledger.epochs.pending_reward_update` until #1071. That field
    // has NO writer on the modern path — every occurrence is a `None`
    // initializer plus one `take()` — so `rupdNext` was unconditionally
    // `null` while cardano-streamer populated it at every epoch, and the
    // single most important field of a reward cross-validation dataset
    // compared vacuously. It is also six fields upstream, not three: the
    // `Some` arm emitted `deltaR1`/`deltaT1`/`totalDistributed` and, worse,
    // put dugite's NET signed `delta_reserves` under the name `deltaR1`,
    // which is the gross expansion.
    //
    // Computed ONCE — this runs the whole member fold, which is ~2.55 s at
    // mainnet scale, so a second call would double every epoch's dump cost.
    // `eta` / `expectedBlocks` used to be read off it and are not any more;
    // see below.
    let forced = dugite_ledger::forced_reward_update(ledger);
    let rupd_next: serde_json::Value = match forced {
        None => serde_json::Value::Null,
        Some(r) => serde_json::json!({
            "deltaR1": r.delta_r1,
            "deltaR2": r.delta_r2,
            "deltaT1": r.delta_t1,
            "rPot": r.r_pot,
            "rewardPot": r.reward_pot,
            "totalDistributed": r.total_distributed,
        }),
    };

    // `expectedBlocks` and `eta` are siblings of `rupdNext` in
    // cardano-streamer's schema. They are NOT fields of any upstream record —
    // `startStep` binds them locally with one use each, so a dump field of
    // these names is necessarily a recomputation from `startStep`'s own inputs,
    // and `start_step_eta` is that recomputation.
    //
    // They USED to be read off the frozen `MonetaryStep`, which reported three
    // things wrong at once. `MonetaryStep.expected_blocks` is post-processed for
    // the division it feeds — clamped to `>= 1`, and set to `0` as a MARKER for
    // the `d >= 4/5` branch — so mainnet epochs 212/213 published `0` where the
    // oracle has 2160/4320. `eta` was then derived from that marker AND capped
    // at 1, so the four epochs where mainnet outran `f * (1 - d)` published
    // `1/1` against the oracle's raw ratio. And both were gated on
    // `forced.is_some()`, which is `None` until a `go` snapshot exists, so the
    // first two Shelley epochs published nothing for two fields that are
    // functions of pparams and `nesBprev` alone.
    //
    // None of the three was a reward-arithmetic defect — every monetary term is
    // byte-identical to the oracle at all four capped epochs, which it could not
    // be if `min 1 eta` were missing where it matters. They were `epochFees`'s
    // class: a definitional mismatch that shows up as a divergence and would
    // MASK a real difference in the same field.
    // Byron emits NOTHING for either, on the same reasoning `epochNonce` below
    // spells out: `startStep` is a Shelley rule and `eta` is a (T)Praos concept,
    // so Byron has no value for it and the oracle is silent for the whole era
    // (`buildSnapshotJson` returns `Nothing`).
    //
    // Computing one anyway would fabricate a value — and it did: the first cut
    // of this change published `eta = 1/1` for all 207 Byron epochs, because
    // `prev_d` defaults to 1/1 there and hits the `d >= 4/5` guard. Uncompared,
    // therefore invisible, therefore exactly the shape of the defect this
    // change exists to remove.
    let start_step_eta = if ledger.era == dugite_primitives::era::Era::Byron {
        None
    } else {
        Some(dugite_ledger::start_step_eta(
            (
                ledger.epochs.prev_d.numerator,
                ledger.epochs.prev_d.denominator,
            ),
            prev_pp.active_slot_coeff_rational(),
            ledger.epochs.snapshots.bprev_blocks_by_pool.values().sum(),
            ledger.epoch_length,
        ))
    };
    let expected_blocks: serde_json::Value = match start_step_eta {
        Some(s) => serde_json::Value::Number(s.expected_blocks.into()),
        None => serde_json::Value::Null,
    };

    // Era-dependent, per cardano-streamer's schema: "`null` for the neutral
    // nonce or Byron era".
    //
    // Byron has no epoch nonce at all — it is a (T)Praos concept — so emitting
    // dugite's zero-initialised `Hash32` there would manufacture a difference
    // against an oracle that correctly says nothing. dugite spells
    // `NeutralNonce` as all-zero (the ledger logs it `NeutralNonce (ZERO)`),
    // which is the same case one layer up, so both render as null rather than
    // as 64 zeros.
    let epoch_nonce: serde_json::Value = {
        let n = ledger.consensus.epoch_nonce.0;
        if ledger.era == dugite_primitives::era::Era::Byron || n.iter().all(|b| *b == 0) {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(hex::encode(n))
        }
    };
    // `null` ONLY where upstream has no value to emit either — `d < 4/5` with
    // `expectedBlocks == 0`, where `blocksMade % expectedBlocks` throws. Any
    // rational put there would be a fabrication that reads as agreement.
    let eta: serde_json::Value = match start_step_eta.and_then(|s| s.eta) {
        Some((num, den)) => serde_json::json!({ "numerator": num, "denominator": den }),
        None => serde_json::Value::Null,
    };

    // RC4: full mark/set/go stake snapshots for cross-validation.
    // Block counts match Haskell's nesBcur/nesBprev (tracked outside snapshots):
    //   mark → nesBcur (blocks produced so far in current epoch)
    //   set  → no blocks (not a Haskell concept)
    //   go   → nesBprev (blocks from previous epoch)
    let snap_mark = ledger
        .epochs
        .snapshots
        .mark
        .as_ref()
        .map(|s| {
            serialize_stake_snapshot(
                "mark",
                s,
                Some(ledger.consensus.epoch_blocks_by_pool.as_ref()),
            )
        })
        .unwrap_or(serde_json::Value::Null);
    let snap_set = ledger
        .epochs
        .snapshots
        .set
        .as_ref()
        .map(|s| serialize_stake_snapshot("set", s, None))
        .unwrap_or(serde_json::Value::Null);
    let snap_go = if let Some(s) = ledger.epochs.snapshots.go.as_ref() {
        serialize_stake_snapshot(
            "go",
            s,
            Some(ledger.epochs.snapshots.bprev_blocks_by_pool.as_ref()),
        )
    } else {
        // In Haskell, snapshots are never null — empty SnapShot with nesBprev blocks.
        let bprev_blocks: serde_json::Map<String, serde_json::Value> = ledger
            .epochs
            .snapshots
            .bprev_blocks_by_pool
            .iter()
            .map(|(pool_id, count)| {
                (
                    hex::encode(pool_id.as_bytes()),
                    serde_json::Value::Number((*count).into()),
                )
            })
            .collect();
        serde_json::json!({
            "name": "go",
            "delegations": {},
            "poolParams": {},
            "stake": {},
            "blocks": bprev_blocks,
        })
    };

    // ── Era-common and era-GATED protocol parameters ────────────────────────
    //
    // The dump published a seven-field subset (rho/tau/d/a0/nOpt/minPoolCost/
    // protocolVersion) — the era-common intersection — so everything an era
    // INTRODUCED compared against nothing. The oracle now emits these
    // (michaeljfazio/cardano-streamer, dugite/full-era-ledger-dumps); these are
    // dugite's matching side, and the shapes are matched deliberately field for
    // field, because a comparison of differently-shaped objects is a schema gap
    // dressed up as a comparison.
    //
    // Era-gated the same way the oracle gates it: ABSENT, not zero, in eras
    // that lack the parameter. Alonzo has no `coinsPerUTxOByte` — that is
    // Babbage's parameter, and Alonzo's `coinsPerUTxOWord` is a different one
    // with a different unit (#919). Emitting a zero for a parameter an era does
    // not have manufactures a value.
    let rat = |r: &dugite_primitives::transaction::Rational| serde_json::json!({ "numerator": r.numerator, "denominator": r.denominator });
    let common_protocol_params = serde_json::json!({
        "minFeeA": prev_pp.min_fee_a,
        "minFeeB": prev_pp.min_fee_b,
        "maxBlockBodySize": prev_pp.max_block_body_size,
        "maxTxSize": prev_pp.max_tx_size,
        "maxBlockHeaderSize": prev_pp.max_block_header_size,
        "keyDeposit": prev_pp.key_deposit.0,
        "poolDeposit": prev_pp.pool_deposit.0,
        "eMax": prev_pp.e_max,
    });
    let era_protocol_params: serde_json::Value = {
        use dugite_primitives::era::Era;
        let era = ledger.era;
        if era < Era::Alonzo {
            serde_json::Value::Null
        } else {
            let mut m = serde_json::Map::new();
            // Alonzo+
            m.insert(
                "costModels".into(),
                serde_json::to_value(&prev_pp.cost_models).unwrap_or(serde_json::Value::Null),
            );
            m.insert(
                "executionUnitPrices".into(),
                serde_json::json!({
                    "priceMemory": rat(&prev_pp.execution_costs.mem_price),
                    "priceSteps": rat(&prev_pp.execution_costs.step_price),
                }),
            );
            m.insert(
                "maxTxExUnits".into(),
                serde_json::json!({
                    "memory": prev_pp.max_tx_ex_units.mem,
                    "steps": prev_pp.max_tx_ex_units.steps,
                }),
            );
            m.insert(
                "maxBlockExUnits".into(),
                serde_json::json!({
                    "memory": prev_pp.max_block_ex_units.mem,
                    "steps": prev_pp.max_block_ex_units.steps,
                }),
            );
            m.insert("maxValueSize".into(), prev_pp.max_val_size.into());
            m.insert(
                "collateralPercentage".into(),
                prev_pp.collateral_percentage.into(),
            );
            m.insert(
                "maxCollateralInputs".into(),
                prev_pp.max_collateral_inputs.into(),
            );
            if era >= Era::Babbage {
                m.insert(
                    "coinsPerUTxOByte".into(),
                    prev_pp.ada_per_utxo_byte.0.into(),
                );
            }
            if era >= Era::Conway {
                // Named keys, not a positional record: #951 shifted six of the
                // ten DRep thresholds and appended `constitution` where
                // `treasuryWithdrawal` belongs. Named keys catch a mislabelled
                // field; an array cannot tell a wrong order from a wrong value.
                m.insert(
                    "poolVotingThresholds".into(),
                    serde_json::json!({
                        "motionNoConfidence": rat(&prev_pp.pvt_motion_no_confidence),
                        "committeeNormal": rat(&prev_pp.pvt_committee_normal),
                        "committeeNoConfidence": rat(&prev_pp.pvt_committee_no_confidence),
                        "hardForkInitiation": rat(&prev_pp.pvt_hard_fork),
                        "ppSecurityGroup": rat(&prev_pp.pvt_pp_security_group),
                    }),
                );
                m.insert(
                    "dRepVotingThresholds".into(),
                    serde_json::json!({
                        "motionNoConfidence": rat(&prev_pp.dvt_no_confidence),
                        "committeeNormal": rat(&prev_pp.dvt_committee_normal),
                        "committeeNoConfidence": rat(&prev_pp.dvt_committee_no_confidence),
                        "updateToConstitution": rat(&prev_pp.dvt_constitution),
                        "hardForkInitiation": rat(&prev_pp.dvt_hard_fork),
                        "ppNetworkGroup": rat(&prev_pp.dvt_pp_network_group),
                        "ppEconomicGroup": rat(&prev_pp.dvt_pp_economic_group),
                        "ppTechnicalGroup": rat(&prev_pp.dvt_pp_technical_group),
                        "ppGovGroup": rat(&prev_pp.dvt_pp_gov_group),
                        "treasuryWithdrawal": rat(&prev_pp.dvt_treasury_withdrawal),
                    }),
                );
                m.insert("committeeMinSize".into(), prev_pp.committee_min_size.into());
                m.insert(
                    "committeeMaxTermLength".into(),
                    prev_pp.committee_max_term_length.into(),
                );
                m.insert(
                    "govActionLifetime".into(),
                    prev_pp.gov_action_lifetime.into(),
                );
                m.insert(
                    "govActionDeposit".into(),
                    prev_pp.gov_action_deposit.0.into(),
                );
                m.insert("dRepDeposit".into(), prev_pp.drep_deposit.0.into());
                m.insert("dRepActivity".into(), prev_pp.drep_activity.into());
                m.insert(
                    "minFeeRefScriptCostPerByte".into(),
                    rat(&prev_pp.min_fee_ref_script_cost_per_byte),
                );
            }
            serde_json::Value::Object(m)
        }
    };

    // `instantaneousRewards` — the PENDING MIR transfers, which the boundary
    // applies and clears.
    //
    // Left as a deliberate gap until now, on the argument that both sides dump
    // at the first block of the new epoch, so the interesting PHASE contains no
    // dump point and emitting it converts a gap into `{}` vs `{}`. That
    // reasoning was about EVIDENCE, and it holds — but a gap also means the
    // field is never compared at all, so a dugite-side value appearing where
    // upstream has none would go unseen. Emitting dugite's real map is not a
    // fabrication: the map genuinely is empty at this point, and if it ever is
    // not, that is exactly what wants to be visible.
    // FOUR keys, and the names are upstream's — `iRReserves`, `iRTreasury`,
    // `deltaReserves`, `deltaTreasury`, the fields of Haskell's
    // `InstantaneousRewards`. Verified against real oracle output rather than
    // assumed: a first cut emitted `reserves`/`treasury`, which pairs with
    // nothing and would have turned one gap into four while looking like a fix.
    let instantaneous_rewards = serde_json::json!({
        "iRReserves": ledger
            .certs
            .pending_mir_reserves
            .iter()
            .map(|(k, v)| (hex::encode(k.as_bytes()), *v))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "iRTreasury": ledger
            .certs
            .pending_mir_treasury
            .iter()
            .map(|(k, v)| (hex::encode(k.as_bytes()), *v))
            .collect::<std::collections::BTreeMap<_, _>>(),
        "deltaReserves": ledger.certs.pending_mir_delta_reserves,
        "deltaTreasury": ledger.certs.pending_mir_delta_treasury,
    });

    // Byron's own shape, paired against the oracle's Byron dump.
    //
    // Byron used to be ORACLE-SILENT — cardano-streamer's `buildSnapshotJson`
    // returned Nothing for the whole era — so dugite's Byron dumps compared
    // against nothing at all. That is 207 mainnet epochs sitting UNDER every
    // Shelley-onward result, and the field that matters most is the circulating
    // supply: the Shelley translation computes
    // `reserves = maxLovelaceSupply - circulating`, so epoch 208's reserves —
    // and therefore every reward calculation in every later era — rests on it.
    //
    // Note what the Shelley-shaped fields report during Byron: `reserves` and
    // `totalStake` hold their GENESIS values for all 207 epochs, because Byron
    // has no reserves concept and dugite derives the real value at the
    // translation. So they are constants here, not measurements — which is
    // precisely why the UTxO has to be reported directly rather than inferred
    // from `totalStake`.
    //
    // `total_lovelace()` folds the whole set; it runs once per epoch boundary at
    // ~715k entries by Byron's end, which is negligible beside the reward fold.
    let byron_utxo: serde_json::Value = if ledger.era == dugite_primitives::era::Era::Byron {
        serde_json::json!({
            "count": ledger.utxo.utxo_set.len(),
            "balance": ledger.utxo.utxo_set.total_lovelace().0,
        })
    } else {
        serde_json::Value::Null
    };

    // Byron-only, matching the oracle's key set for the era. `lastSlot` and the
    // genesis delegation count are Byron ledger state; `byronProtocolParams` is
    // the genesis-derived fee policy and size limits.
    let (byron_last_slot, byron_delegation, byron_protocol_params) =
        if ledger.era == dugite_primitives::era::Era::Byron {
            (
                serde_json::json!(ledger.tip.point.slot().map(|s| s.0).unwrap_or(0)),
                serde_json::json!({ "count": ledger.genesis_delegates.len() }),
                byron_pparams.unwrap_or(serde_json::Value::Null),
            )
        } else {
            (
                serde_json::Value::Null,
                serde_json::Value::Null,
                serde_json::Value::Null,
            )
        };

    serde_json::json!({
        "epoch": epoch,
        "utxo": byron_utxo,
        "lastSlot": byron_last_slot,
        "byronDelegation": byron_delegation,
        "byronProtocolParams": byron_protocol_params,
        "commonProtocolParams": common_protocol_params,
        "eraProtocolParams": era_protocol_params,
        "instantaneousRewards": instantaneous_rewards,
        "epochFees": epoch_fees,
        "reserves": ledger.epochs.reserves.0,
        "treasury": ledger.epochs.treasury.0,
        "totalStake": total_stake,
        "activeStake": active_stake,
        "totalPools": pool_distribution.len(),
        "poolDistribution": pool_distribution,
        "snapshotEraName": format!("{}", ledger.era),
        "epochNonce": epoch_nonce,
        "eta": eta,
        "expectedBlocks": expected_blocks,
        "deposits": {
            "stakeKey": deposit_stake_key,
            "pool": deposit_pool,
            "dRep": deposit_drep,
            "proposal": deposit_proposal,
            "total": deposit_total,
        },
        // `proposals` and `enactedRoots` USED to be emitted here and are not
        // any more. Neither has an upstream counterpart:
        // `extractConwayGovData` emits exactly five keys and neither is among
        // them, so each was an unconditional SCHEMA GAP in every Conway epoch —
        // a dugite-only debugging field holding the comparison at exit 2 for
        // ~140 epochs it could never say anything about.
        //
        // `enactedRoots` was not merely unpaired, it was WRONG in two ways that
        // only real Conway oracle output showed: the roots belong inside
        // `conwayGov.nextEnactState.prevGovActionIds`, where they are the
        // pulser's self-inclusive copy rather than the live one, and each id
        // renders as `{govActionIx, txId}` rather than dugite's `txid#ix`
        // string. Both are now emitted there.
        //
        // `proposals` is dropped outright rather than kept behind a comparator
        // exclusion: an exclusion is for a field that IS compared and whose
        // difference is known-benign, and this one has nothing to compare
        // against. The live proposal set remains queryable over N2C
        // (`GetProposals`), which is where a debugging reader should get it.
        "conwayGov": conway_gov,
        "protocolParams": protocol_params,
        "rupdNext": rupd_next,
        "rupdApplied": prev_rupd_next,
        "snapshots": {
            "mark": snap_mark,
            "set": snap_set,
            "go": snap_go,
        },
    })
}

/// Write an epoch snapshot either to a per-epoch file in `output_dir` (when set)
/// or as an NDJSON line to the shared `output` writer (fallback).
fn write_epoch_snapshot(
    snapshot: &serde_json::Value,
    epoch: u64,
    output_dir: &Option<std::path::PathBuf>,
    output: &mut Box<dyn std::io::Write>,
) -> anyhow::Result<()> {
    use std::io::Write;

    if let Some(dir) = output_dir {
        // Write {epoch}.json — pretty-printed for human readability.
        let path = dir.join(format!("{epoch}.json"));
        let file = std::fs::File::create(&path)
            .map_err(|e| anyhow::anyhow!("Cannot create {}: {e}", path.display()))?;
        let writer = std::io::BufWriter::new(file);
        serde_json::to_writer_pretty(writer, snapshot)
            .map_err(|e| anyhow::anyhow!("JSON serialise error: {e}"))?;
    } else {
        // NDJSON: one compact JSON object per line.
        serde_json::to_writer(&mut *output, snapshot)
            .map_err(|e| anyhow::anyhow!("JSON write error: {e}"))?;
        writeln!(output).map_err(|e| anyhow::anyhow!("Write error: {e}"))?;
    }
    Ok(())
}

async fn run_db_command(args: DbArgs) -> Result<()> {
    match args.command {
        DbCommand::Info(info_args) => run_db_info(info_args).await,
    }
}

async fn run_db_info(args: DbInfoArgs) -> Result<()> {
    let db_path = &args.database_path;
    if !db_path.exists() {
        anyhow::bail!("Database path does not exist: {}", db_path.display());
    }

    let storage_profile: dugite_storage::StorageProfile = args
        .storage_profile
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let storage_config = dugite_storage::config::resolve_storage_config(
        storage_profile,
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    // Genuinely read-only: `open_with_config` enters WRITE mode, and this
    // command has no genesis file, so it would record a guessed slots-per-chunk
    // into the database and lock the real node out (#1081).
    let chain_db = dugite_storage::ChainDB::open_read_only(db_path, &storage_config.immutable)?;

    // Immutable DB info
    let immutable_dir = db_path.join("immutable");
    let (chunk_count, immutable_size) = if immutable_dir.exists() {
        let mut count = 0u64;
        let mut total_size = 0u64;
        for entry in std::fs::read_dir(&immutable_dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.ends_with(".chunk") {
                count += 1;
            }
            total_size += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
        (count, total_size)
    } else {
        (0, 0)
    };

    // VolatileDB block count (from ChainDB tip info)
    let volatile_count = chain_db.volatile_block_count();

    // Ledger snapshot info
    let snapshot_dir = db_path.join("snapshots");
    let (snapshot_count, snapshot_size) = if snapshot_dir.exists() {
        let mut count = 0u64;
        let mut total_size = 0u64;
        for entry in std::fs::read_dir(&snapshot_dir)? {
            let entry = entry?;
            count += 1;
            total_size += entry.metadata().map(|m| m.len()).unwrap_or(0);
        }
        (count, total_size)
    } else {
        (0, 0)
    };

    let tip = chain_db.get_tip();

    println!("Dugite Database Info");
    println!("=====================");
    println!("  Database path:      {}", db_path.display());
    println!(
        "  Chain tip slot:     {}",
        tip.point.slot().map(|s| s.0).unwrap_or(0)
    );
    println!("  Chain tip block:    {}", tip.block_number.0);
    println!();
    println!("ImmutableDB:");
    println!("  Chunk files:        {chunk_count}");
    println!("  Total size:         {}", format_size(immutable_size));
    println!();
    println!("VolatileDB:");
    println!("  Block count:        {volatile_count}");
    println!();
    println!("Ledger Snapshots:");
    println!("  Snapshot count:     {snapshot_count}");
    println!("  Total size:         {}", format_size(snapshot_size));

    Ok(())
}

fn format_size(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB ({bytes} bytes)", b / GB)
    } else if b >= MB {
        format!("{:.2} MB ({bytes} bytes)", b / MB)
    } else if b >= KB {
        format!("{:.2} KB ({bytes} bytes)", b / KB)
    } else {
        format!("{bytes} bytes")
    }
}

async fn run_mithril_import(args: MithrilImportArgs) -> Result<()> {
    info!(
        "Starting Mithril snapshot import for network magic {}",
        args.network_magic
    );

    // `--no-include-ancillary` is a convenience alias for
    // `--include-ancillary=false`. They are `conflicts_with` so at most
    // one is set — combine them into the final boolean here.
    let include_ancillary = args.include_ancillary && !args.no_include_ancillary;

    if !include_ancillary {
        info!(
            "Ancillary archive download disabled by --no-include-ancillary; \
             ledger state will be derived entirely from chunk-by-chunk replay"
        );
    }

    mithril::import_snapshot(
        args.network_magic,
        &args.database_path,
        args.temp_dir.as_deref(),
        args.mithril_genesis_vkey.as_deref(),
        args.skip_certificate_verification,
        args.allow_stale_pparams,
        include_ancillary,
    )
    .await
}

async fn run_node(args: RunArgs, log_handle: Option<logging::LogHandle>) -> Result<()> {
    info!(
        version = env!("CARGO_PKG_VERSION"),
        "Dugite Cardano Node starting"
    );

    // Load configuration
    let mut node_config = config::NodeConfig::load(&args.config)?;
    let config_dir = args
        .config
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .to_path_buf();

    // CLI override for the Dijkstra genesis path.  Mirrors cardano-node's
    // `--dijkstra-genesis` flag; takes precedence over the JSON
    // `DijkstraGenesisFile` config field when provided. Issue #462 Phase 6.
    if let Some(ref cli_path) = args.dijkstra_genesis {
        node_config.dijkstra_genesis_file = Some(cli_path.to_string_lossy().into_owned());
    }

    node_config.validate(&config_dir)?;

    // Apply config-file log verbosity at startup.
    //
    // `logging::init` runs in `main()` *before* the config file is parsed, so
    // the initial filter is seeded only from `--log-level`/`RUST_LOG` (default
    // `info`). Without this step the config's `MinSeverity`/`LogDirective`
    // would have no effect until a SIGHUP reload — an operator who sets
    // `MinSeverity: Debug` in config.json and starts the node would silently
    // get INFO. Now that the config is loaded, apply it via the live reload
    // handle, matching cardano-node (where MinSeverity drives startup
    // verbosity).
    //
    // Precedence (highest first): `RUST_LOG` env > explicit `--log-level` CLI >
    // config `LogDirective` > config `MinSeverity`. An explicit CLI/env
    // override is therefore never clobbered by the config file.
    if let Some(handle) = log_handle.as_ref() {
        let cli_or_env_override =
            args.log.log_level.is_some() || std::env::var_os("RUST_LOG").is_some();
        if !cli_or_env_override {
            let directive = node_config.log_directive.clone().unwrap_or_else(|| {
                logging::min_severity_to_directive(&node_config.min_severity).to_string()
            });
            match handle.reload(&directive) {
                Ok(()) => {
                    info!(directive = %directive, "Applied config-file log verbosity at startup")
                }
                Err(e) => tracing::warn!(
                    directive = %directive,
                    "Failed to apply config-file log verbosity: {e}"
                ),
            }
        }
    }

    // Resolve effective metrics port using priority (highest first):
    //   1. --no-metrics flag → 0 (disabled)
    //   2. --metrics-port <PORT> CLI arg → explicit operator override (wins
    //      even over TurnOnLogMetrics=false)
    //   3. TurnOnLogMetrics=false in config JSON → 0 (master off-switch,
    //      matching cardano-node)
    //   4. MetricsPort field in config JSON → site-wide default from config file
    //   5. Dugite default: config::DEFAULT_METRICS_PORT (12796 — avoids
    //      collision with cardano-node's 12798)
    //
    // Single implementation in config.rs, exercised directly by its tests
    // (#941: this used to be duplicated, and the copy in the test module had
    // drifted on both the default port and the TurnOnLogMetrics branch).
    let effective_metrics_port: u16 = config::resolve_metrics_port(
        args.no_metrics,
        args.metrics_port,
        node_config.turn_on_log_metrics,
        node_config.metrics_port,
    );

    // Resolve effective UTxO RPC config (#672 M1.A). See
    // config::resolve_rpc for the precedence table.
    let rpc_config = config::resolve_rpc(
        args.no_rpc,
        args.rpc_host.as_deref(),
        args.rpc_port,
        node_config.rpc.as_ref(),
    )
    .map_err(|e| anyhow::anyhow!("invalid RPC config: {e}"))?;
    if let Some(ref rc) = rpc_config {
        info!(
            bind = %rc.bind,
            port = rc.port,
            reflection = rc.reflection_enabled,
            web = rc.web_enabled,
            alpha = rc.alpha_enabled,
            tls = rc.tls.is_some(),
            "UTxO RPC (gRPC) server enabled"
        );
    } else {
        info!("UTxO RPC (gRPC) server disabled");
    }

    // Load topology
    let topology = topology::Topology::load(&args.topology)?;
    let all_peers = topology.all_peers();

    info!(config = %args.config.display(), "Configuration");
    info!(path = %args.database_path.display(), "Database");
    info!(path = %args.socket_path.display(), "Socket");
    info!(
        network = ?node_config.network,
        magic = node_config.network_magic.unwrap_or_else(|| node_config.network.magic()),
        "Network",
    );
    info!(host = %args.host_addr, port = args.port, "Listen");
    if effective_metrics_port > 0 {
        info!(port = effective_metrics_port, "Metrics");
    } else {
        info!("Metrics disabled");
    }
    info!(
        total = all_peers.len(),
        producers = topology.producers.len(),
        bootstrap = topology.bootstrap_peers.as_ref().map_or(0, |v| v.len()),
        local = topology
            .local_roots
            .iter()
            .map(|g| g.access_points.len())
            .sum::<usize>(),
        public = topology
            .public_roots
            .iter()
            .map(|r| r.access_points.len())
            .sum::<usize>(),
        "Topology",
    );

    // Resolve storage configuration: profile < config file < CLI
    let storage_profile: dugite_storage::StorageProfile = args
        .storage_profile
        .parse()
        .map_err(|e: String| anyhow::anyhow!(e))?;
    let storage_config = dugite_storage::config::resolve_storage_config(
        storage_profile,
        node_config.storage.as_ref(),
        args.immutable_index_type.as_deref(),
        args.utxo_backend.as_deref(),
        args.utxo_memtable_size_mb,
        args.utxo_block_cache_size_mb,
        args.utxo_bloom_filter_bits,
    )
    .map_err(|e| anyhow::anyhow!(e))?;

    info!(
        profile = %storage_profile,
        index = ?storage_config.immutable.index_type,
        utxo = ?storage_config.utxo.backend,
        "Storage",
    );

    // Resolve effective consensus mode (#535).  CLI flag wins; otherwise the
    // JSON config field `ConsensusMode` is canonical, matching cardano-node.
    let (consensus_mode_str, consensus_mode_source) =
        config::resolve_consensus_mode(args.consensus_mode.as_deref(), node_config.consensus_mode);
    let consensus_mode = consensus_mode_str.to_string();
    info!(
        mode = %consensus_mode,
        source = consensus_mode_source,
        "ConsensusMode resolved",
    );

    // Initialize the node
    let mut node = node::Node::new(node::NodeArgs {
        config: node_config,
        topology,
        topology_path: args.topology.clone(),
        config_path: args.config.clone(),
        database_path: args.database_path,
        socket_path: args.socket_path,
        host_addr: args.host_addr,
        port: args.port,
        config_dir,
        shelley_kes_key: args.shelley_kes_key,
        shelley_vrf_key: args.shelley_vrf_key,
        shelley_operational_certificate: args.shelley_operational_certificate,
        _shelley_cold_key: args.shelley_cold_key,
        rpc_config,
        metrics_port: effective_metrics_port,
        require_metrics: args.require_metrics,
        compat_metrics: args.compat_metrics,
        liveness_threshold_secs: args.liveness_threshold_secs,
        mempool_max_tx: args.mempool_max_tx,
        mempool_max_bytes: args.mempool_max_bytes,
        snapshot_max_retained: args.snapshot_max_retained,
        snapshot_bulk_min_blocks: args.snapshot_bulk_min_blocks,
        snapshot_bulk_min_secs: args.snapshot_bulk_min_secs,
        storage_config,
        consensus_mode,
        validate_all_blocks: args.validate_all_blocks,
        skip_eagerly_validated_header_crypto: args.skip_eagerly_validated_header_crypto,
        log_handle,
    })?;

    info!("");

    // node.run() registers its own SIGINT/SIGTERM handlers and performs
    // graceful shutdown internally (peer demotion, storage flush, ledger
    // snapshot).  Do NOT race it with an outer select! — that drops the
    // run future mid-shutdown and leaves spawned tasks (metrics, N2C/N2N
    // connections) alive, causing the process to hang.
    node.run().await?;

    Ok(())
}

/// Shapes the preprod cross-validation could NOT reach.
///
/// The conwayGov emission was validated against 22 epochs of real
/// cardano-streamer output, but preprod exercised only some of each sum type:
/// its whole committee is script-credentialled and never resigns, its
/// constitution always has a guardrail script, and no `NoConfidence` ever
/// enacted. Those arms are taken from the cardano-ledger `ToJSON` instances
/// rather than from observed data, so they are pinned here — an unobserved arm
/// that nothing asserts is a guess with no way to notice it went wrong, which
/// is exactly how the `enactedRoots` string format survived 16 null epochs.
#[cfg(test)]
mod conway_gov_shape_tests {
    use super::{committee_json, constitution_json, credential_key, gov_action_id_json};
    use dugite_primitives::hash::Hash32;
    use dugite_primitives::time::EpochNo;
    use dugite_primitives::transaction::{Anchor, Constitution, GovActionId, Rational};

    /// Byte 28 is the credential-kind discriminator, and BOTH arms matter.
    /// preprod only ever exercised `scriptHash`; mainnet's committee is
    /// key-credentialled, so the other arm is what the tip run will use.
    #[test]
    fn credential_key_renders_both_kinds() {
        let mut key = [0u8; 32];
        key[..28].copy_from_slice(&[0xab; 28]);
        assert_eq!(
            credential_key(&Hash32::from_bytes(key)),
            format!("keyHash-{}", "ab".repeat(28)),
        );

        key[28] = 0x01;
        assert_eq!(
            credential_key(&Hash32::from_bytes(key)),
            format!("scriptHash-{}", "ab".repeat(28)),
        );
    }

    /// `SNothing` committee — after an enacted `NoConfidence` — is `null`, not
    /// an object with an empty member map.
    #[test]
    fn committee_is_null_without_a_threshold() {
        let members = imbl::HashMap::new();
        assert!(committee_json(&members, None).is_null());
    }

    #[test]
    fn committee_renders_members_and_threshold() {
        let mut members = imbl::HashMap::new();
        let mut key = [0u8; 32];
        key[..28].copy_from_slice(&[0x11; 28]);
        key[28] = 0x01;
        members.insert(Hash32::from_bytes(key), EpochNo(229));
        let t = Rational {
            numerator: 2,
            denominator: 3,
        };
        let v = committee_json(&members, Some(&t));
        assert_eq!(v["members"][format!("scriptHash-{}", "11".repeat(28))], 229);
        assert_eq!(v["threshold"]["numerator"], 2);
        assert_eq!(v["threshold"]["denominator"], 3);
    }

    /// The guardrail-less constitution OMITS `script` — upstream builds the
    /// pair list with a comprehension guard, so the key is absent rather than
    /// null. Emitting `"script": null` would be a schema gap on one side in
    /// every epoch of a chain that has no guardrail.
    #[test]
    fn constitution_omits_the_script_key_when_absent() {
        let c = Constitution {
            anchor: Anchor {
                url: "ipfs://x".to_string(),
                data_hash: Hash32::from_bytes([0x22; 32]),
            },
            script_hash: None,
        };
        let v = constitution_json(Some(&c));
        assert!(
            v.get("script").is_none(),
            "script must be ABSENT, not null: {v}"
        );
        assert_eq!(v["anchor"]["url"], "ipfs://x");
        assert_eq!(v["anchor"]["dataHash"], "22".repeat(32));
        assert!(constitution_json(None).is_null());
    }

    /// A gov action id is an OBJECT. dugite emitted `"<txid>#<ix>"` under a
    /// top-level key upstream does not have, and 16 consecutive all-null
    /// preprod epochs would have "confirmed" either format.
    #[test]
    fn gov_action_id_is_an_object_not_a_string() {
        let id = GovActionId {
            transaction_id: Hash32::from_bytes([0x33; 32]),
            action_index: 7,
        };
        let v = gov_action_id_json(Some(&id));
        assert_eq!(v["txId"], "33".repeat(32));
        assert_eq!(v["govActionIx"], 7);
        assert!(gov_action_id_json(None).is_null());
    }
}

#[cfg(test)]
mod digest_tests {
    use super::digest_of_map;

    /// The Rust and Python digests MUST agree byte for byte.
    ///
    /// dugite emits `{__count__, __sum__, __digest__}` directly under
    /// `DUGITE_DUMP_DIGEST=1`; cardano-streamer emits the raw map and
    /// `scripts/validation/diff-cstreamer-dumps.py` reduces it. If the two
    /// canonical forms drift, every `stake` and `delegations` path — 98% of the
    /// payload — reports as divergent while the data is identical, and the
    /// noise would mask a real difference behind a field that is already red.
    ///
    /// These vectors were produced by the Python implementation and pasted
    /// here. Regenerate with
    /// `scripts/validation/diff-cstreamer-dumps.py`'s `digest_of_map`.
    #[test]
    fn digest_of_map_matches_the_comparator() {
        let mut stake = serde_json::Map::new();
        stake.insert(format!("keyHash-{}", "aa".repeat(28)), 5.into());
        stake.insert(format!("keyHash-{}", "bb".repeat(28)), 7.into());
        stake.insert(format!("scriptHash-{}", "cc".repeat(28)), 11.into());
        let got = digest_of_map(&stake);
        assert_eq!(got["__count__"], 3);
        assert_eq!(got["__sum__"], 23, "integer maps must carry a total");
        assert_eq!(
            got["__digest__"],
            "007b710d3b7632b2323a3b1e648e8608ee70810a15d432d9cc8ae4a45a83a6cb"
        );

        // String values (delegations: credential -> pool id) must NOT produce a
        // `__sum__`. A zero there would read as a real total of zero stake.
        let mut dele = serde_json::Map::new();
        dele.insert(
            format!("keyHash-{}", "aa".repeat(28)),
            serde_json::Value::String("cc".repeat(28)),
        );
        dele.insert(
            format!("keyHash-{}", "bb".repeat(28)),
            serde_json::Value::String("dd".repeat(28)),
        );
        let got = digest_of_map(&dele);
        assert_eq!(got["__count__"], 2);
        assert!(
            got.get("__sum__").is_none(),
            "a non-integer map must omit __sum__, not report 0"
        );
        assert_eq!(
            got["__digest__"],
            "3888cb0e8d7ffb2154ae738ae35a885928a41432ac1d9c9df804e7204dcbdcd1"
        );
    }

    /// Key ORDER must not change the digest, and a one-lovelace change MUST.
    ///
    /// The first is why the canonical form sorts; the second is the whole
    /// reason a digest is an acceptable substitute for the raw map.
    #[test]
    fn digest_is_order_independent_but_value_sensitive() {
        let mut a = serde_json::Map::new();
        a.insert("keyHash-02".into(), 2.into());
        a.insert("keyHash-01".into(), 1.into());
        let mut b = serde_json::Map::new();
        b.insert("keyHash-01".into(), 1.into());
        b.insert("keyHash-02".into(), 2.into());
        assert_eq!(
            digest_of_map(&a)["__digest__"],
            digest_of_map(&b)["__digest__"]
        );

        let mut c = serde_json::Map::new();
        c.insert("keyHash-01".into(), 1.into());
        c.insert("keyHash-02".into(), 3.into());
        assert_ne!(
            digest_of_map(&a)["__digest__"],
            digest_of_map(&c)["__digest__"],
            "a one-unit change in one entry must change the digest"
        );
    }
}
