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
    if let Some(ref genesis_path) = node_config.byron_genesis_file {
        let genesis_path = config_dir.join(genesis_path);
        if let Ok((genesis, _hash)) = genesis::ByronGenesis::load_with_hash(&genesis_path) {
            let k = genesis.security_param();
            byron_epoch_length = 10 * k;
            byron_slot_duration_ms = genesis.slot_duration_ms();
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
            let snapshot =
                build_epoch_snapshot(&ledger, current_epoch, epoch_fees, max_lovelace_supply);

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

    // Dump final epoch (the current in-progress epoch at the stop point).
    if blocks_applied > 0 && last_epoch != u64::MAX {
        let snapshot = build_epoch_snapshot(&ledger, last_epoch, epoch_fees, max_lovelace_supply);

        write_epoch_snapshot(&snapshot, last_epoch, &args.output_dir, &mut output)?;
        epochs_written += 1;
    }

    let elapsed = start_time.elapsed();
    info!(
        blocks = blocks_applied,
        epochs_written,
        elapsed_secs = elapsed.as_secs(),
        "dump-snapshot complete"
    );

    Ok(())
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
/// Render cardano-ledger's on-wire IPv6 bytes as a standard address.
///
/// The 16 bytes are NOT a network-order address: cardano-ledger encodes an
/// `IPv6` as its four `Word32`s in LITTLE-ENDIAN order, so each 4-byte group
/// arrives reversed. dugite stores them verbatim, which is right — but
/// hex-dumping them printed `f804012a2c41910100000000030000 00` where
/// cardano-node prints `2a01:4f8:191:412c::3`
/// (`instance ToJSON IPv6 where toJSON = toJSON . show`,
/// cardano-ledger-core `Cardano/Ledger/Orphans.hs`). Same datum, and the
/// difference is a per-4-byte-group reversal — verified against every IPv6
/// relay on mainnet epochs 208-215, 6 of 6 reproduced exactly.
///
/// Note the sibling `ipv4` was already rendered as dotted-quad text in the same
/// expression, so the dump was internally inconsistent as well as wrong.
fn ipv6_from_ledger_bytes(b: &[u8; 16]) -> std::net::Ipv6Addr {
    let mut be = [0u8; 16];
    for i in 0..4 {
        let w = u32::from_le_bytes([b[i * 4], b[i * 4 + 1], b[i * 4 + 2], b[i * 4 + 3]]);
        be[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    std::net::Ipv6Addr::from(be)
}

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
                serde_json::json!({
                    "credential": { "keyHash": cred_hex },
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
                        "ipv6": ipv6.map(|b| ipv6_from_ledger_bytes(&b).to_string()),
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

    let mut result = serde_json::json!({
        "name": name,
        "epoch": snapshot.epoch.0,
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

    // DRep distribution for cross-validation.
    let (drep_cache, drep_no_conf, drep_abstain_val) = ledger.build_drep_power_cache();
    let mut drep_distr_map = serde_json::Map::new();
    for (hash, power) in &drep_cache {
        drep_distr_map.insert(
            format!("drep-keyHash-{}", &hash.to_hex()[..30]),
            serde_json::Value::Number((*power).into()),
        );
    }
    drep_distr_map.insert(
        "drep-alwaysNoConfidence".to_string(),
        serde_json::Value::Number(drep_no_conf.into()),
    );
    drep_distr_map.insert(
        "drep-alwaysAbstain".to_string(),
        serde_json::Value::Number(drep_abstain_val.into()),
    );
    let drep_distr = serde_json::Value::Object(drep_distr_map);

    // Proposal details for cross-validation debugging.
    let proposal_details: Vec<serde_json::Value> = ledger
        .gov.governance
        .proposals
        .iter()
        .map(|(id, state)| {
            serde_json::json!({
                "txId": id.transaction_id.to_hex(),
                "index": id.action_index,
                "expiresEpoch": state.expires_epoch.0,
                "proposedEpoch": state.proposed_epoch.0,
                "deposit": state.procedure.deposit.0,
                "actionType": match &state.procedure.gov_action {
                    dugite_primitives::transaction::GovAction::ParameterChange { .. } => "ParameterChange",
                    dugite_primitives::transaction::GovAction::HardForkInitiation { .. } => "HardForkInitiation",
                    dugite_primitives::transaction::GovAction::TreasuryWithdrawals { .. } => "TreasuryWithdrawals",
                    dugite_primitives::transaction::GovAction::NoConfidence { .. } => "NoConfidence",
                    dugite_primitives::transaction::GovAction::UpdateCommittee { .. } => "UpdateCommittee",
                    dugite_primitives::transaction::GovAction::NewConstitution { .. } => "NewConstitution",
                    dugite_primitives::transaction::GovAction::InfoAction => "InfoAction",
                },
            })
        })
        .collect();

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
    // mainnet scale, so a second call to populate `eta`/`expectedBlocks` would
    // double every epoch's dump cost.
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

    // `expectedBlocks` and `eta` are siblings of `rupdNext` in cardano-streamer's
    // schema and fall out of the same frozen monetary step, so they are emitted
    // from it rather than recomputed.
    let expected_blocks = forced.map(|r| r.expected_blocks).unwrap_or(0);

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
    let blocks_made: u64 = ledger.epochs.snapshots.bprev_blocks_by_pool.values().sum();
    let eta: serde_json::Value = match forced {
        // `expected_blocks == 0` is start_step_monetary's marker for the
        // `d >= 4/5` branch, where Haskell sets eta = 1 outright.
        Some(_) if expected_blocks == 0 => serde_json::json!({
            "numerator": 1, "denominator": 1
        }),
        Some(_) => {
            // eta = min(1, blocksMade / expectedBlocks), as an exact rational.
            let (num, den) = if blocks_made >= expected_blocks {
                (1u64, 1u64)
            } else {
                let g = gcd(blocks_made.max(1), expected_blocks);
                (blocks_made / g, expected_blocks / g)
            };
            serde_json::json!({ "numerator": num, "denominator": den })
        }
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
            "epoch": 0,
            "delegations": {},
            "poolParams": {},
            "stake": {},
            "blocks": bprev_blocks,
        })
    };

    serde_json::json!({
        "epoch": epoch,
        "epochFees": epoch_fees,
        "reserves": ledger.epochs.reserves.0,
        "treasury": ledger.epochs.treasury.0,
        "totalStake": total_stake,
        "activeStake": active_stake,
        "totalPools": pool_distribution.len(),
        "poolDistribution": pool_distribution,
        "snapshotEraName": format!("{}", ledger.era),
        "enactedRoots": {
            "PParamUpdate": ledger.gov.governance.enacted_pparam_update.as_ref()
                .map(|id| format!("{}#{}", id.transaction_id.to_hex(), id.action_index)),
            "HardFork": ledger.gov.governance.enacted_hard_fork.as_ref()
                .map(|id| format!("{}#{}", id.transaction_id.to_hex(), id.action_index)),
            "Committee": ledger.gov.governance.enacted_committee.as_ref()
                .map(|id| format!("{}#{}", id.transaction_id.to_hex(), id.action_index)),
            "Constitution": ledger.gov.governance.enacted_constitution.as_ref()
                .map(|id| format!("{}#{}", id.transaction_id.to_hex(), id.action_index)),
        },
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
        "proposals": proposal_details,
        "drepDistr": drep_distr,
        "protocolParams": protocol_params,
        "rupdNext": rupd_next,
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

    // Open the ChainDB read-only
    let chain_db = dugite_storage::ChainDB::open_with_config(
        db_path,
        &storage_config.immutable,
        dugite_storage::chain_db::DEFAULT_SECURITY_PARAM_K,
    )?;

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

#[cfg(test)]
mod digest_tests {
    use super::{digest_of_map, ipv6_from_ledger_bytes};

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
    /// cardano-ledger encodes an `IPv6` as four LITTLE-ENDIAN `Word32`s, so the
    /// wire bytes are the address with each 4-byte group reversed.
    ///
    /// The vector is a REAL mainnet relay — the fifth entry of pool
    /// `bcc34d3c45cd3b8770c75c91c3023a9146aa505c4bd5cf094dae9acc` at epoch 212,
    /// whose address cardano-streamer renders as `2a01:4f8:191:412c::3`. A
    /// synthetic address would not have caught this: the bug is a byte ORDER,
    /// and a palindromic or all-zero test value is invariant under it.
    #[test]
    fn ipv6_renders_ledger_little_endian_words_as_an_address() {
        let wire: [u8; 16] = [
            0xf8, 0x04, 0x01, 0x2a, 0x2c, 0x41, 0x91, 0x01, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x00,
        ];
        assert_eq!(
            ipv6_from_ledger_bytes(&wire).to_string(),
            "2a01:4f8:191:412c::3",
            "IPv6 relay must render as cardano-node's `show`, not as raw hex"
        );

        // The naive reading — treat the wire bytes as a network-order address —
        // must NOT produce the same string, or the test would pass without the
        // conversion doing anything.
        assert_ne!(
            std::net::Ipv6Addr::from(wire).to_string(),
            "2a01:4f8:191:412c::3",
            "if these agree the vector is order-invariant and proves nothing"
        );
    }

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
