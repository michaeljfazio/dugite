use anyhow::{Context, Result};
use dugite_primitives::block::ProtocolVersion;
use dugite_primitives::network::NetworkId;
use dugite_storage::StorageConfigJson;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::path::Path;

/// Consensus protocol mode.
///
/// Matches cardano-node's `ConsensusMode`:
/// - `PraosMode`: Standard Ouroboros Praos operation.
/// - `GenesisMode`: Enables Ouroboros Genesis for trustless bulk sync.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConsensusMode {
    /// Standard Ouroboros Praos operation.
    ///
    /// JSON: `"Praos"` — the cardano-node 11.0.1 canonical spelling
    /// (`Cardano.Node.Types` accepts exactly `"Genesis"` / `"Praos"`).
    /// `"PraosMode"` is accepted as a legacy dugite alias.
    #[default]
    #[serde(rename = "Praos", alias = "PraosMode")]
    PraosMode,
    /// Ouroboros Genesis for trustless bulk sync from untrusted peers.
    ///
    /// JSON: `"Genesis"` (canonical), `"GenesisMode"` (legacy alias).
    #[serde(rename = "Genesis", alias = "GenesisMode")]
    GenesisMode,
}

impl ConsensusMode {
    /// Lower-case string identifier matching the dugite-node `--consensus-mode`
    /// CLI flag values (`"praos"` / `"genesis"`).
    pub fn as_runtime_str(self) -> &'static str {
        match self {
            ConsensusMode::PraosMode => "praos",
            ConsensusMode::GenesisMode => "genesis",
        }
    }
}

/// Low-level Ouroboros Genesis tuning knobs.
///
/// Mirrors cardano-node's `LowLevelGenesisOptions` config object, parsed into
/// `GenesisConfigFlags` (ouroboros-consensus `Ouroboros.Consensus.Node.Genesis`).
/// Field names and defaults are byte-for-byte those of cardano-node 11.0.1
/// (`Cardano.Node.Orphans` `FromJSON GenesisConfigFlags`):
///
/// ```json
/// { "EnableCSJ": true, "EnableLoEAndGDD": true, "EnableLoP": true,
///   "BlockFetchGracePeriod": 10, "BucketCapacity": 100000,
///   "BucketRate": 500, "CSJJumpSize": 4320, "GDDRateLimit": 1.0 }
/// ```
///
/// Only consulted when `ConsensusMode` is `Genesis` (Praos mode is
/// `disableGenesisConfig`: every subsystem off).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LowLevelGenesisOptions {
    /// Enable ChainSync Jumping (`gcfEnableCSJ`, default true).
    #[serde(rename = "EnableCSJ", default = "default_true")]
    pub enable_csj: bool,
    /// Enable the Limit on Eagerness + Genesis Density Disconnection
    /// (`gcfEnableLoEAndGDD`, default true).
    #[serde(rename = "EnableLoEAndGDD", default = "default_true")]
    pub enable_loe_and_gdd: bool,
    /// Enable the Limit on Patience leaky bucket (`gcfEnableLoP`, default true).
    #[serde(rename = "EnableLoP", default = "default_true")]
    pub enable_lop: bool,
    /// BlockFetch bulk-sync grace period in seconds before rotating a
    /// starving peer (`gcfBlockFetchGracePeriod`; upstream default 10 s).
    #[serde(rename = "BlockFetchGracePeriod", default)]
    pub block_fetch_grace_period_secs: Option<f64>,
    /// LoP bucket capacity in tokens (`gcfBucketCapacity`; default 100 000).
    #[serde(rename = "BucketCapacity", default)]
    pub bucket_capacity: Option<u64>,
    /// LoP bucket leak rate in tokens/second (`gcfBucketRate`; default 500).
    #[serde(rename = "BucketRate", default)]
    pub bucket_rate: Option<u64>,
    /// CSJ jump size in slots (`gcfCSJJumpSize`; default 2*2160 = 4320, the
    /// Byron forecast range).
    #[serde(rename = "CSJJumpSize", default)]
    pub csj_jump_size: Option<u64>,
    /// Minimum seconds between GDD evaluations (`gcfGDDRateLimit`; default 1.0).
    #[serde(rename = "GDDRateLimit", default)]
    pub gdd_rate_limit_secs: Option<f64>,
}

fn default_true() -> bool {
    true
}

impl Default for LowLevelGenesisOptions {
    fn default() -> Self {
        LowLevelGenesisOptions {
            enable_csj: true,
            enable_loe_and_gdd: true,
            enable_lop: true,
            block_fetch_grace_period_secs: None,
            bucket_capacity: None,
            bucket_rate: None,
            csj_jump_size: None,
            gdd_rate_limit_secs: None,
        }
    }
}

impl LowLevelGenesisOptions {
    /// `gbfcGracePeriod` with the `mkGenesisConfig` default applied (10 s).
    pub fn effective_block_fetch_grace_period_secs(&self) -> f64 {
        self.block_fetch_grace_period_secs.unwrap_or(10.0)
    }

    /// `csbcCapacity` with the upstream default applied (100 000 tokens).
    pub fn effective_bucket_capacity(&self) -> u64 {
        self.bucket_capacity.unwrap_or(100_000)
    }

    /// `csbcRate` with the upstream default applied (500 tokens/s).
    pub fn effective_bucket_rate(&self) -> u64 {
        self.bucket_rate.unwrap_or(500)
    }

    /// `csjcJumpSize` with the upstream default applied (4320 slots).
    pub fn effective_csj_jump_size(&self) -> u64 {
        self.csj_jump_size.unwrap_or(2 * 2160)
    }

    /// `lgpGDDRateLimit` with the upstream default applied (1.0 s).
    pub fn effective_gdd_rate_limit_secs(&self) -> f64 {
        self.gdd_rate_limit_secs.unwrap_or(1.0)
    }
}

/// Resolve the effective consensus mode (#535).
///
/// Precedence: explicit CLI flag (`--consensus-mode`) wins; otherwise the
/// JSON config field `ConsensusMode` is canonical (mirroring cardano-node).
///
/// Returns `(mode, source)` where `source` is `"cli"` or `"config"` for the
/// startup log line — operators must be able to see which source won.
///
/// Invalid CLI values (anything other than `"praos"` / `"genesis"`) are
/// rejected by `clap`'s `value_parser` before reaching this function.
pub fn resolve_consensus_mode(
    cli: Option<&str>,
    config: ConsensusMode,
) -> (&'static str, &'static str) {
    match cli {
        Some("genesis") => ("genesis", "cli"),
        Some("praos") => ("praos", "cli"),
        Some(_) => (config.as_runtime_str(), "config"),
        None => (config.as_runtime_str(), "config"),
    }
}

/// Resolve the effective UTxO RPC config from CLI + JSON config —
/// issue #672.
///
/// Precedence (highest first):
///   1. `--no-rpc` → server disabled (returns `None`).
///   2. `--rpc-host` / `--rpc-port` set → server enabled, CLI values
///      override config values.
///   3. `config.rpc.enabled` true → server enabled with the config-file
///      values.
///   4. Otherwise → server disabled (returns `None`).
///
/// Returns the constructed `dugite_rpc::RpcConfig` ready to hand to
/// [`dugite_rpc::RpcServer::start`], or `None` if the server should not
/// start.
pub fn resolve_rpc(
    no_rpc: bool,
    cli_host: Option<&str>,
    cli_port: Option<u16>,
    config: Option<&RpcConfigJson>,
) -> Result<Option<dugite_rpc::RpcConfig>, String> {
    if no_rpc {
        return Ok(None);
    }

    // CLI presence forces enable; otherwise fall back to config.enabled.
    let cli_present = cli_host.is_some() || cli_port.is_some();
    let cfg_enabled = config.map(|c| c.enabled).unwrap_or(false);
    if !cli_present && !cfg_enabled {
        return Ok(None);
    }

    let mut out = dugite_rpc::RpcConfig::default();

    if let Some(cfg) = config {
        if let Some(ref addr) = cfg.listen_addr {
            out.bind = addr
                .parse::<std::net::IpAddr>()
                .map_err(|e| format!("Rpc.ListenAddr is not a valid IP address: {e}"))?;
        }
        if let Some(p) = cfg.port {
            out.port = p;
        }
        if let Some(n) = cfg.max_concurrent_streams {
            out.max_concurrent_streams = n;
        }
        if let Some(n) = cfg.stream_buffer_size {
            out.stream_buffer = n;
        }
        if let Some(b) = cfg.reflection_enabled {
            out.reflection_enabled = b;
        }
        if let Some(b) = cfg.web_enabled {
            out.web_enabled = b;
        }
        if let Some(b) = cfg.alpha_enabled {
            out.alpha_enabled = b;
        }
        if let Some(ref tls) = cfg.tls {
            out.tls = Some(dugite_rpc::RpcTlsConfig {
                cert_path: tls.cert_path.clone(),
                key_path: tls.key_path.clone(),
            });
        }
    }

    if let Some(host) = cli_host {
        out.bind = host
            .parse::<std::net::IpAddr>()
            .map_err(|e| format!("--rpc-host is not a valid IP address: {e}"))?;
    }
    if let Some(port) = cli_port {
        out.port = port;
    }

    Ok(Some(out))
}

/// Inbound connection limits (matches Haskell AcceptedConnectionsLimit).
///
/// Haskell's hand-written `FromJSON` instance uses short keys (`hardLimit`,
/// `softLimit`, `delay`).  We expose those as the primary names and accept the
/// old long camelCase names as aliases for backward compatibility.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct AcceptedConnectionsLimit {
    /// Refuse new inbound connections beyond this count.
    #[serde(
        alias = "acceptedConnectionsHardLimit",
        rename = "hardLimit",
        default = "default_hard_limit"
    )]
    pub hard_limit: u32,
    /// Start delaying new connections at this count.
    #[serde(
        alias = "acceptedConnectionsSoftLimit",
        rename = "softLimit",
        default = "default_soft_limit"
    )]
    pub soft_limit: u32,
    /// Max delay in seconds applied to connections above soft limit.
    #[serde(
        alias = "acceptedConnectionsDelay",
        rename = "delay",
        default = "default_conn_delay"
    )]
    pub delay: f64,
}

impl Default for AcceptedConnectionsLimit {
    fn default() -> Self {
        Self {
            hard_limit: 512,
            soft_limit: 384,
            delay: 5.0,
        }
    }
}

/// Diffusion mode — controls whether the node accepts inbound N2N connections.
///
/// Matches cardano-node's `DiffusionMode` config field:
/// - `InitiatorAndResponder` (default): full P2P mode — opens listening port
///   and accepts inbound connections.
/// - `InitiatorOnly`: node only makes outbound connections, never listens for
///   inbound.  Advertises `initiator_only = true` in the N2N handshake so
///   remote peers do not attempt reverse connections.  Typical for block
///   producers behind a firewall.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiffusionMode {
    /// Only initiate outbound connections (block producer behind NAT/firewall).
    InitiatorOnly,
    /// Both initiate outbound and accept inbound connections (relay).
    #[default]
    InitiatorAndResponder,
}

impl fmt::Display for DiffusionMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InitiatorOnly => write!(f, "InitiatorOnly"),
            Self::InitiatorAndResponder => write!(f, "InitiatorAndResponder"),
        }
    }
}

/// Node configuration (compatible with cardano-node config format)
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct NodeConfig {
    /// Network identifier
    #[serde(default = "default_network")]
    pub network: NetworkId,

    /// Network magic number
    #[serde(default)]
    pub network_magic: Option<u64>,

    /// Protocol parameters (can be a string like "Cardano" or a struct)
    #[serde(default, deserialize_with = "deserialize_protocol")]
    pub protocol: Protocol,

    /// RequiresNetworkMagic at top level (guild/newer configs)
    #[serde(default)]
    pub requires_network_magic: Option<String>,

    /// Shelley genesis file path
    #[serde(default)]
    pub shelley_genesis_file: Option<String>,

    /// Byron genesis file path
    #[serde(default)]
    pub byron_genesis_file: Option<String>,

    /// Alonzo genesis file path
    #[serde(default)]
    pub alonzo_genesis_file: Option<String>,

    /// Conway genesis file path
    #[serde(default)]
    pub conway_genesis_file: Option<String>,

    /// Dijkstra genesis file path (post-Conway HFC; carries pparams 34-37).
    ///
    /// Mirrors cardano-node's `DijkstraGenesisFile` config field. Parsed
    /// via `dugite_primitives::genesis::DijkstraGenesis`; not yet wired
    /// into runtime ledger rules (issue #462 Phase 6 — parse only).
    #[serde(default)]
    pub dijkstra_genesis_file: Option<String>,

    /// Expected Blake2b-256 hash of the Byron genesis file (hex string)
    #[serde(default)]
    pub byron_genesis_hash: Option<String>,

    /// Expected Blake2b-256 hash of the Shelley genesis file (hex string)
    #[serde(default)]
    pub shelley_genesis_hash: Option<String>,

    /// Expected Blake2b-256 hash of the Alonzo genesis file (hex string)
    #[serde(default)]
    pub alonzo_genesis_hash: Option<String>,

    /// Expected Blake2b-256 hash of the Conway genesis file (hex string)
    #[serde(default)]
    pub conway_genesis_hash: Option<String>,

    /// Expected Blake2b-256 hash of the Dijkstra genesis file (hex string).
    ///
    /// Mirrors cardano-node's `DijkstraGenesisHash` config field.
    #[serde(default)]
    pub dijkstra_genesis_hash: Option<String>,

    /// Diffusion mode — controls inbound connection acceptance.
    ///
    /// `"InitiatorAndResponder"` (default): full relay mode, accepts inbound.
    /// `"InitiatorOnly"`: block producer behind NAT, outbound only.
    /// Matches cardano-node's `DiffusionMode` config field.
    #[serde(default)]
    pub diffusion_mode: DiffusionMode,

    /// Enable peer sharing mini-protocol (default: `None` = auto).
    ///
    /// When `None`, peer sharing is automatically disabled for block producers
    /// (when `--shelley-kes-key` is provided) and enabled for relays — matching
    /// the Haskell cardano-node default behaviour.  Set explicitly to override.
    #[serde(default)]
    pub peer_sharing: Option<bool>,

    /// Target number of root peers (default: 60, matching cardano-node)
    #[serde(default = "default_root_peers")]
    pub target_number_of_root_peers: usize,

    /// Target number of active peers (default: 20, matching cardano-node)
    #[serde(default = "default_active_peers")]
    pub target_number_of_active_peers: usize,

    /// Target number of established peers (default: 30, matching cardano-node)
    #[serde(default = "default_established_peers")]
    pub target_number_of_established_peers: usize,

    /// Target number of known peers (default: 150, matching cardano-node)
    #[serde(default = "default_known_peers")]
    pub target_number_of_known_peers: usize,

    /// Target number of active big ledger peers (default: 5, matching cardano-node)
    #[serde(default = "default_active_big_ledger_peers")]
    pub target_number_of_active_big_ledger_peers: usize,

    /// Target number of established big ledger peers (default: 10, matching cardano-node)
    #[serde(default = "default_established_big_ledger_peers")]
    pub target_number_of_established_big_ledger_peers: usize,

    /// Target number of known big ledger peers (default: 15, matching cardano-node)
    #[serde(default = "default_known_big_ledger_peers")]
    pub target_number_of_known_big_ledger_peers: usize,

    /// Trace options
    #[serde(default)]
    pub trace_options: TraceOptions,

    /// Minimum severity for logging
    #[serde(default = "default_min_severity")]
    pub min_severity: String,

    /// Tracing filter directive in `tracing_subscriber::EnvFilter` syntax
    /// (e.g. `"info,dugite_network=trace,dugite_consensus=debug"`).
    ///
    /// If set, this overrides the global level on **SIGHUP reload** (#473) —
    /// operators can edit the config file at runtime, send `SIGHUP`, and the
    /// per-subsystem trace verbosity is reloaded without a process restart.
    ///
    /// If absent, the initial `--log-level` CLI flag value remains in effect.
    /// The initial process startup does not yet read this field; only SIGHUP
    /// applies it. (Operators wanting startup-time effect can pass the same
    /// directive via the `RUST_LOG` env var, which is honoured by `--log-level`.)
    #[serde(default)]
    pub log_directive: Option<String>,

    /// Prometheus metrics port.
    ///
    /// When set to 0 the metrics server is disabled.  The CLI flag
    /// `--metrics-port` takes precedence over this field; the CLI flag
    /// `--no-metrics` forces the port to 0 regardless of this value.
    /// If neither the CLI flag nor this field is present the node falls back
    /// to 12798, matching cardano-node's default.
    #[serde(default)]
    pub metrics_port: Option<u16>,

    /// Storage configuration (optional overrides for storage profiles)
    #[serde(default)]
    pub storage: Option<StorageConfigJson>,

    /// UTxO RPC (gRPC) server configuration — issue #672.
    ///
    /// `None` (or absent) leaves the RPC server disabled. When present
    /// the `Enabled` field gates startup; CLI flags `--no-rpc`,
    /// `--rpc-host`, `--rpc-port` override. See `resolve_rpc()` for the
    /// full precedence table.
    #[serde(default)]
    pub rpc: Option<RpcConfigJson>,

    /// Governor churn interval during normal (caught-up) operation, in seconds.
    ///
    /// Controls how often the governor rotates a random subset of peers to
    /// ensure the node does not become permanently attached to the same set.
    /// Matches cardano-node default of 3300 s (55 minutes).
    #[serde(default = "default_churn_interval_normal_secs")]
    pub churn_interval_normal_secs: u64,

    /// Governor churn interval during syncing, in seconds.
    ///
    /// Faster rotation while syncing so that the node can quickly shed
    /// unresponsive peers.  Matches cardano-node default of 900 s (15 minutes).
    #[serde(default = "default_churn_interval_sync_secs")]
    pub churn_interval_sync_secs: u64,

    /// Number of consecutive governor evaluation cycles in which a hot peer
    /// must serve zero new blocks before it is demoted back to warm (stall
    /// detection).  A cycle runs every 30 seconds, so the default of 6 cycles
    /// corresponds to a 3-minute stall window.
    #[serde(default = "default_stall_demotion_cycles")]
    pub stall_demotion_cycles: u32,

    /// Failure count threshold above which a hot peer is unconditionally
    /// demoted to warm during each governor evaluation cycle.  Local root
    /// peers are exempt from this check and will never be demoted by the
    /// governor.  Default: 5 failures.
    #[serde(default = "default_error_demotion_threshold")]
    pub error_demotion_threshold: u32,

    /// Enable experimental hard fork transitions (default: false).
    ///
    /// When true, the node signals `ProtVer 11 0` in forged block headers,
    /// advertising readiness for the next major protocol version (Dijkstra era).
    /// When false (default), the node signals `ProtVer 10 8` — the maximum
    /// Conway-era protocol version supported by this software release.
    ///
    /// Matches cardano-node's `ExperimentalHardForksEnabled` config field.
    /// Must remain false on mainnet unless instructed otherwise.
    #[serde(default)]
    pub experimental_hard_forks_enabled: bool,

    /// Consensus protocol mode (`"Praos"` or `"Genesis"`).
    #[serde(default)]
    pub consensus_mode: ConsensusMode,

    /// Low-level Ouroboros Genesis tuning (cardano-node
    /// `LowLevelGenesisOptions`). Only consulted in Genesis mode; absent ⇒
    /// upstream defaults (`defaultGenesisConfigFlags`).
    #[serde(default)]
    pub low_level_genesis_options: Option<LowLevelGenesisOptions>,

    /// Path to a lightweight-checkpoints JSON file (cardano-node
    /// `CheckpointsFile`): `{"checkpoints":[{"blockNo":N,"hash":hex},...]}`.
    /// Resolved relative to the config file's directory. Checkpoints are
    /// enforced for every header in both consensus modes.
    #[serde(rename = "CheckpointsFile", default)]
    pub checkpoints_file: Option<String>,

    /// Optional Blake2b-256 hex of the checkpoints file bytes
    /// (`CheckpointsFileHash`); a mismatch is a fatal startup error.
    #[serde(rename = "CheckpointsFileHash", default)]
    pub checkpoints_file_hash: Option<String>,

    // ── Genesis mode sync targets ──────────────────────────────────────
    /// Active peers during Genesis bulk sync (default: 5, matching cardano-node).
    #[serde(default = "default_sync_active_peers")]
    pub sync_target_number_of_active_peers: usize,
    /// Established peers during Genesis bulk sync (default: 10, matching cardano-node).
    #[serde(default = "default_sync_established_peers")]
    pub sync_target_number_of_established_peers: usize,
    /// Known peers during Genesis bulk sync (default: 150, matching cardano-node).
    #[serde(default = "default_sync_known_peers")]
    pub sync_target_number_of_known_peers: usize,
    /// Root peers during Genesis bulk sync (default: 0).
    #[serde(default)]
    pub sync_target_number_of_root_peers: usize,
    /// Active big ledger peers during Genesis bulk sync (default: 30).
    #[serde(default = "default_sync_active_blp")]
    pub sync_target_number_of_active_big_ledger_peers: usize,
    /// Established big ledger peers during Genesis bulk sync (default: 40, matching cardano-node).
    #[serde(default = "default_sync_established_blp")]
    pub sync_target_number_of_established_big_ledger_peers: usize,
    /// Known big ledger peers during Genesis bulk sync (default: 100).
    #[serde(default = "default_sync_known_blp")]
    pub sync_target_number_of_known_big_ledger_peers: usize,
    /// Pause sync if active BLPs drop below this (Genesis safety gate, default: 5).
    #[serde(default = "default_min_blp_trusted")]
    pub min_big_ledger_peers_for_trusted_state: usize,

    // ── Connection management ──────────────────────────────────────────
    /// Inbound connection limits (hard/soft/delay).
    #[serde(default)]
    pub accepted_connections_limit: Option<AcceptedConnectionsLimit>,
    /// Time before idle mini-protocol connection is pruned (seconds, default: 5).
    ///
    /// Accepts fractional seconds, matching Haskell's `DiffTime` type.
    #[serde(default = "default_protocol_idle_timeout")]
    pub protocol_idle_timeout: f64,
    /// Connection TIME_WAIT duration after close (seconds, default: 60).
    ///
    /// Accepts fractional seconds, matching Haskell's `DiffTime` type.
    #[serde(default = "default_time_wait_timeout")]
    pub time_wait_timeout: f64,
    /// Outbound governor poll interval (seconds, default: 0).
    ///
    /// 0 means the governor runs as fast as events arrive (Haskell default).
    /// Accepts fractional seconds, matching Haskell's `DiffTime` type.
    #[serde(default = "default_egress_poll_interval")]
    pub egress_poll_interval: f64,
    /// ChainSync-specific idle timeout (seconds, 0 = no timeout).
    ///
    /// Accepts fractional seconds, matching Haskell's `DiffTime` type.
    #[serde(default)]
    pub chain_sync_idle_timeout: Option<f64>,

    // ── Inbound rate limiting (G1) ─────────────────────────────────────
    /// Maximum N2N inbound connections per source IP within a 60-second window.
    ///
    /// Set to 0 to disable per-IP rate limiting entirely (not recommended).
    /// Default: 5 — matches Haskell ouroboros-network `InboundGovernor`
    /// `connectionRateLimit`.  Prevents a single source IP from exhausting all
    /// inbound connection slots with stalled half-open connections.
    ///
    /// Note: this config field was previously defined in `ConnectionManagerConfig`
    /// in `dugite-network` but was never wired into the accept loop.  This is the
    /// authoritative config field going forward.
    #[serde(default = "default_per_ip_rate_limit_n2n")]
    pub per_ip_rate_limit_n2n: usize,

    /// Maximum concurrent N2C (Unix socket) connections.
    ///
    /// Default: 16.  Prevents a local attacker or misbehaving wallet from
    /// accumulating unbounded JoinHandles in the N2C accept loop (G3).
    #[serde(default = "default_max_n2c_connections")]
    pub max_n2c_connections: usize,

    /// Maximum blocks pulled by a single BlockFetch `MsgRequestRange` (bulk sync).
    ///
    /// A larger range amortises the request round-trip across more blocks,
    /// which helps tiny-block Byron bulk sync; the actual range is still sized
    /// adaptively by an 8 MiB byte budget so large Conway blocks shrink it.
    /// `None` (the default) uses the maximum — the network `MAX_BLOCKS_PER_FETCH`
    /// cap (2000).  Any value is clamped to `[64, 2000]` at use; it can never
    /// exceed the network per-batch DoS cap.  The `DUGITE_BLOCKFETCH_MAX_RANGE`
    /// environment variable overrides this field when set.
    #[serde(default, rename = "BlockFetchMaxRange")]
    pub blockfetch_max_range: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct Protocol {
    #[serde(default = "default_requires_network_magic")]
    pub requires_network_magic: String,
}

/// UTxO RPC server configuration — issue #672.
///
/// Mirrors `dugite_rpc::RpcConfig` for the JSON wire format with
/// PascalCase keys to match the surrounding cardano-node-style config.
/// `Enabled` defaults to `false` so an `Rpc` block present in the file
/// must opt-in to start the server.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct RpcConfigJson {
    /// Whether to start the RPC server. Overridden by `--rpc-port`/
    /// `--rpc-host` (forces on) and `--no-rpc` (forces off).
    #[serde(default)]
    pub enabled: bool,
    /// IP address to bind. Defaults to `127.0.0.1` (loopback only).
    #[serde(default)]
    pub listen_addr: Option<String>,
    /// TCP port. Defaults to `dugite_rpc::config::DEFAULT_RPC_PORT` (50051).
    #[serde(default)]
    pub port: Option<u16>,
    /// Maximum concurrent HTTP/2 streams per connection.
    #[serde(default)]
    pub max_concurrent_streams: Option<u32>,
    /// Per-stream buffer size (events).
    #[serde(default)]
    pub stream_buffer_size: Option<usize>,
    /// Expose gRPC reflection. Defaults to true.
    #[serde(default)]
    pub reflection_enabled: Option<bool>,
    /// Accept gRPC-Web (HTTP/1.1). Defaults to false.
    #[serde(default)]
    pub web_enabled: Option<bool>,
    /// Expose v1alpha services in addition to v1beta. Defaults to true.
    #[serde(default)]
    pub alpha_enabled: Option<bool>,
    /// Optional TLS termination.
    #[serde(default)]
    pub tls: Option<RpcTlsConfigJson>,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct RpcTlsConfigJson {
    pub cert_path: std::path::PathBuf,
    pub key_path: std::path::PathBuf,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "PascalCase")]
pub struct TraceOptions {
    #[serde(default)]
    pub trace_block_fetch_client: bool,
    #[serde(default)]
    pub trace_block_fetch_server: bool,
    #[serde(default)]
    pub trace_chain_db: bool,
    #[serde(default)]
    pub trace_chain_sync_client: bool,
    #[serde(default)]
    pub trace_chain_sync_server: bool,
    #[serde(default)]
    pub trace_forge: bool,
    #[serde(default)]
    pub trace_mempool: bool,
}

fn default_network() -> NetworkId {
    NetworkId::Mainnet
}

fn default_root_peers() -> usize {
    60
}

fn default_active_peers() -> usize {
    20 // Haskell cardano-node default
}

fn default_established_peers() -> usize {
    30 // Haskell cardano-node default
}

fn default_known_peers() -> usize {
    150 // Haskell cardano-node default
}

fn default_active_big_ledger_peers() -> usize {
    5
}

fn default_established_big_ledger_peers() -> usize {
    10
}

fn default_known_big_ledger_peers() -> usize {
    15
}

fn default_churn_interval_normal_secs() -> u64 {
    3300 // 55 minutes, matching cardano-node
}

fn default_churn_interval_sync_secs() -> u64 {
    900 // 15 minutes, matching cardano-node
}

fn default_stall_demotion_cycles() -> u32 {
    6 // 6 × 30 s = 3 minutes of inactivity triggers demotion
}

fn default_error_demotion_threshold() -> u32 {
    5 // 5 accumulated failures triggers demotion
}

fn default_hard_limit() -> u32 {
    512
}

fn default_soft_limit() -> u32 {
    384
}

fn default_conn_delay() -> f64 {
    5.0
}

fn default_sync_active_peers() -> usize {
    5 // Haskell cardano-node default
}

fn default_sync_established_peers() -> usize {
    10 // Haskell cardano-node default
}

fn default_sync_known_peers() -> usize {
    150 // Haskell cardano-node default
}

fn default_sync_active_blp() -> usize {
    30
}

fn default_sync_established_blp() -> usize {
    40 // Haskell cardano-node default
}

fn default_sync_known_blp() -> usize {
    100
}

fn default_min_blp_trusted() -> usize {
    5
}

fn default_protocol_idle_timeout() -> f64 {
    5.0
}

fn default_time_wait_timeout() -> f64 {
    60.0
}

/// Haskell default is 0 — governor runs on-demand without a fixed poll interval.
fn default_egress_poll_interval() -> f64 {
    0.0
}

fn default_min_severity() -> String {
    "Info".to_string()
}

fn default_requires_network_magic() -> String {
    "RequiresMagic".to_string()
}

/// Default N2N per-IP rate limit: 5 connections per 60-second window.
///
/// Matches Haskell ouroboros-network's `InboundGovernor` `connectionRateLimit`.
fn default_per_ip_rate_limit_n2n() -> usize {
    5
}

/// Default maximum concurrent N2C connections (G3).
fn default_max_n2c_connections() -> usize {
    16
}

/// Deserialize Protocol from either a string (e.g. "Cardano") or a struct
fn deserialize_protocol<'de, D>(deserializer: D) -> Result<Protocol, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de;

    struct ProtocolVisitor;

    impl<'de> de::Visitor<'de> for ProtocolVisitor {
        type Value = Protocol;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a string or Protocol object")
        }

        fn visit_str<E: de::Error>(self, _value: &str) -> Result<Protocol, E> {
            Ok(Protocol::default())
        }

        fn visit_map<M: de::MapAccess<'de>>(self, map: M) -> Result<Protocol, M::Error> {
            Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_any(ProtocolVisitor)
}

/// Validate peer selection target consistency for one target set.
///
/// Mirrors Haskell's `sanePeerSelectionTargets` from
/// `Ouroboros.Network.PeerSelection.Governor.Types`.  The function is called
/// for **both** the deadline and the Genesis sync target sets, unconditionally
/// (i.e. without checking `ConsensusMode`).
///
/// The 14 predicates checked are:
///
/// Ordering invariants (cold ≥ warm ≥ hot):
///   1.  active     <= established
///   2.  established<= known
///   3.  root       <= known
///   4.  active_blp <= established_blp
///   5.  established_blp <= known_blp
///
/// Upper-bound safety limits (prevent runaway resource usage):
///   6.  active     <= 100
///   7.  established <= 1000
///   8.  known      <= 10000
///   9.  active_blp <= 100
///   10. established_blp <= 1000
///   11. known_blp  <= 10000
///
/// (The ≥ 0 checks are implicit because all fields are `usize`.)
///
/// `label` is a short prefix used in the error message ("deadline" or "sync").
#[allow(clippy::too_many_arguments)]
fn sane_peer_selection_targets(
    label: &str,
    active: usize,
    established: usize,
    known: usize,
    root: usize,
    active_blp: usize,
    established_blp: usize,
    known_blp: usize,
) -> Result<()> {
    // ── Ordering invariants ───────────────────────────────────────────────────
    if active > established {
        anyhow::bail!("[{label}] active ({active}) must be <= established ({established})");
    }
    if established > known {
        anyhow::bail!("[{label}] established ({established}) must be <= known ({known})");
    }
    if root > known {
        anyhow::bail!("[{label}] root ({root}) must be <= known ({known})");
    }
    if active_blp > established_blp {
        anyhow::bail!(
            "[{label}] active_big_ledger_peers ({active_blp}) must be <= \
             established_big_ledger_peers ({established_blp})"
        );
    }
    if established_blp > known_blp {
        anyhow::bail!(
            "[{label}] established_big_ledger_peers ({established_blp}) must be <= \
             known_big_ledger_peers ({known_blp})"
        );
    }

    // ── Upper-bound safety limits ─────────────────────────────────────────────
    const MAX_ACTIVE: usize = 100;
    const MAX_ESTABLISHED: usize = 1000;
    const MAX_KNOWN: usize = 10_000;

    if active > MAX_ACTIVE {
        anyhow::bail!("[{label}] active ({active}) exceeds maximum allowed ({MAX_ACTIVE})");
    }
    if established > MAX_ESTABLISHED {
        anyhow::bail!(
            "[{label}] established ({established}) exceeds maximum allowed ({MAX_ESTABLISHED})"
        );
    }
    if known > MAX_KNOWN {
        anyhow::bail!("[{label}] known ({known}) exceeds maximum allowed ({MAX_KNOWN})");
    }
    if active_blp > MAX_ACTIVE {
        anyhow::bail!(
            "[{label}] active_big_ledger_peers ({active_blp}) exceeds maximum allowed \
             ({MAX_ACTIVE})"
        );
    }
    if established_blp > MAX_ESTABLISHED {
        anyhow::bail!(
            "[{label}] established_big_ledger_peers ({established_blp}) exceeds maximum \
             allowed ({MAX_ESTABLISHED})"
        );
    }
    if known_blp > MAX_KNOWN {
        anyhow::bail!(
            "[{label}] known_big_ledger_peers ({known_blp}) exceeds maximum allowed \
             ({MAX_KNOWN})"
        );
    }

    Ok(())
}

impl NodeConfig {
    /// Returns the protocol version this node should stamp on forged block headers.
    ///
    /// This is a **software capability signal**, not the on-chain protocol version.
    /// It tells the network the maximum protocol version this node supports.
    ///
    /// Matches cardano-node's `cardanoProtocolVersion` in `Cardano.Node.Protocol.Cardano.hs`:
    /// - `ExperimentalHardForksEnabled = false` → `ProtVer 11 0`  (signals Conway readiness)
    /// - `ExperimentalHardForksEnabled = true`  → `ProtVer 12 0`  (Dijkstra, preview testnet)
    ///
    /// Values from current master branch (2026-05-07):
    ///   if npcExperimentalHardForksEnabled then ProtVer (natVersion @12) 0
    ///                                      else ProtVer (natVersion @11) 0
    ///
    /// `maxMajorProtVer` is derived from this: nodes that claim ProtVer 11 will accept
    /// blocks with on-chain ProtVer <= 11; those claiming ProtVer 12 accept <= 12.
    /// For preview testnet (Dijkstra active, ProtVer 12), set ExperimentalHardForksEnabled=true.
    pub fn node_protocol_version(&self) -> ProtocolVersion {
        if self.experimental_hard_forks_enabled {
            ProtocolVersion {
                major: 12,
                minor: 0,
            }
        } else {
            ProtocolVersion {
                major: 11,
                minor: 0,
            }
        }
    }

    /// Returns the maximum major protocol version this node can validate.
    ///
    /// Derived from the node protocol version's major component.
    /// Used by the Praos consensus layer for the obsolete-node envelope check:
    /// if the on-chain ledger protocol version exceeds this, the node rejects
    /// all block headers (forcing an upgrade).
    pub fn max_major_protocol_version(&self) -> u64 {
        self.node_protocol_version().major
    }

    pub fn load(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = std::fs::read_to_string(path)
                .with_context(|| format!("Failed to read config file: {}", path.display()))?;

            // Try JSON first (cardano-node format), then TOML
            if path.extension().is_some_and(|e| e == "json") {
                serde_json::from_str(&content)
                    .with_context(|| format!("Failed to parse JSON config: {}", path.display()))
            } else {
                toml::from_str(&content)
                    .with_context(|| format!("Failed to parse TOML config: {}", path.display()))
            }
        } else {
            // Use defaults
            Ok(Self::default())
        }
    }

    /// Get effective network magic (from explicit field or network default)
    #[cfg(test)]
    pub fn network_magic(&self) -> u64 {
        self.network_magic.unwrap_or_else(|| self.network.magic())
    }

    /// Resolve effective peer sharing setting.
    ///
    /// If `peer_sharing` is explicitly set in the config, returns that value.
    /// Otherwise, returns `false` for block producers (when `is_block_producer`
    /// is true) and `true` for relays — matching Haskell cardano-node defaults.
    pub fn effective_peer_sharing(&self, is_block_producer: bool) -> bool {
        self.peer_sharing.unwrap_or(!is_block_producer)
    }

    /// Validate configuration at startup: check genesis file existence and hash formats.
    /// `config_dir` is the directory containing the config file, used to resolve
    /// relative genesis file paths.
    pub fn validate(&self, config_dir: &Path) -> Result<()> {
        // ── Peer target sanity checks (Haskell: sanePeerSelectionTargets) ─────
        //
        // Haskell cardano-node calls `sanePeerSelectionTargets` at startup for
        // BOTH the deadline targets and the Genesis sync targets, regardless of
        // consensus mode.  Violation causes a startup failure.
        //
        // Reference: ouroboros-network/src/Ouroboros/Network/PeerSelection/Governor/Types.hs
        sane_peer_selection_targets(
            "deadline",
            self.target_number_of_active_peers,
            self.target_number_of_established_peers,
            self.target_number_of_known_peers,
            self.target_number_of_root_peers,
            self.target_number_of_active_big_ledger_peers,
            self.target_number_of_established_big_ledger_peers,
            self.target_number_of_known_big_ledger_peers,
        )?;

        sane_peer_selection_targets(
            "sync",
            self.sync_target_number_of_active_peers,
            self.sync_target_number_of_established_peers,
            self.sync_target_number_of_known_peers,
            self.sync_target_number_of_root_peers,
            self.sync_target_number_of_active_big_ledger_peers,
            self.sync_target_number_of_established_big_ledger_peers,
            self.sync_target_number_of_known_big_ledger_peers,
        )?;

        let genesis_files: &[(&str, &Option<String>, &Option<String>)] = &[
            ("Byron", &self.byron_genesis_file, &self.byron_genesis_hash),
            (
                "Shelley",
                &self.shelley_genesis_file,
                &self.shelley_genesis_hash,
            ),
            (
                "Alonzo",
                &self.alonzo_genesis_file,
                &self.alonzo_genesis_hash,
            ),
            (
                "Conway",
                &self.conway_genesis_file,
                &self.conway_genesis_hash,
            ),
            (
                "Dijkstra",
                &self.dijkstra_genesis_file,
                &self.dijkstra_genesis_hash,
            ),
        ];

        for (era, file_opt, hash_opt) in genesis_files {
            if let Some(ref file_path) = file_opt {
                let resolved = config_dir.join(file_path);
                if !resolved.exists() {
                    anyhow::bail!(
                        "{era} genesis file not found: {} (resolved from config dir {})",
                        resolved.display(),
                        config_dir.display()
                    );
                }
            }
            if let Some(ref hash_hex) = hash_opt {
                if hash_hex.len() != 64 || !hash_hex.chars().all(|c| c.is_ascii_hexdigit()) {
                    anyhow::bail!(
                        "{era} genesis hash is not a valid 64-character hex string: {hash_hex}"
                    );
                }
            }
        }

        Ok(())
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        NodeConfig {
            network: NetworkId::Mainnet,
            network_magic: None,
            protocol: Protocol::default(),
            requires_network_magic: None,
            shelley_genesis_file: None,
            byron_genesis_file: None,
            alonzo_genesis_file: None,
            conway_genesis_file: None,
            dijkstra_genesis_file: None,
            byron_genesis_hash: None,
            shelley_genesis_hash: None,
            alonzo_genesis_hash: None,
            conway_genesis_hash: None,
            dijkstra_genesis_hash: None,
            diffusion_mode: DiffusionMode::default(),
            peer_sharing: None,
            target_number_of_root_peers: 60,
            target_number_of_active_peers: 20,
            target_number_of_established_peers: 30,
            target_number_of_known_peers: 150,
            target_number_of_active_big_ledger_peers: 5,
            target_number_of_established_big_ledger_peers: 10,
            target_number_of_known_big_ledger_peers: 15,
            trace_options: TraceOptions::default(),
            min_severity: "Info".to_string(),
            log_directive: None,
            metrics_port: None,
            storage: None,
            rpc: None,
            churn_interval_normal_secs: default_churn_interval_normal_secs(),
            churn_interval_sync_secs: default_churn_interval_sync_secs(),
            stall_demotion_cycles: default_stall_demotion_cycles(),
            error_demotion_threshold: default_error_demotion_threshold(),
            experimental_hard_forks_enabled: false,
            consensus_mode: ConsensusMode::default(),
            low_level_genesis_options: None,
            checkpoints_file: None,
            checkpoints_file_hash: None,
            sync_target_number_of_active_peers: 5,
            sync_target_number_of_established_peers: 10,
            sync_target_number_of_known_peers: 150,
            sync_target_number_of_root_peers: 0,
            sync_target_number_of_active_big_ledger_peers: default_sync_active_blp(),
            sync_target_number_of_established_big_ledger_peers: default_sync_established_blp(),
            sync_target_number_of_known_big_ledger_peers: default_sync_known_blp(),
            min_big_ledger_peers_for_trusted_state: default_min_blp_trusted(),
            accepted_connections_limit: None,
            protocol_idle_timeout: default_protocol_idle_timeout(),
            time_wait_timeout: default_time_wait_timeout(),
            egress_poll_interval: default_egress_poll_interval(),
            chain_sync_idle_timeout: None,
            per_ip_rate_limit_n2n: default_per_ip_rate_limit_n2n(),
            max_n2c_connections: default_max_n2c_connections(),
            blockfetch_max_range: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = NodeConfig::default();
        assert_eq!(config.network, NetworkId::Mainnet);
        assert_eq!(config.network_magic(), 764824073);
    }

    #[test]
    fn test_custom_magic() {
        let config = NodeConfig {
            network_magic: Some(42),
            ..NodeConfig::default()
        };
        assert_eq!(config.network_magic(), 42);
    }

    #[test]
    fn test_validate_default_config_passes() {
        let config = NodeConfig::default();
        assert!(config.validate(Path::new(".")).is_ok());
    }

    #[test]
    fn test_validate_missing_genesis_file() {
        let config = NodeConfig {
            shelley_genesis_file: Some("nonexistent-genesis.json".to_string()),
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("Shelley genesis file not found"));
    }

    #[test]
    fn test_validate_invalid_genesis_hash_too_short() {
        let config = NodeConfig {
            byron_genesis_hash: Some("abcdef".to_string()),
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("not a valid 64-character hex"));
    }

    #[test]
    fn test_validate_invalid_genesis_hash_non_hex() {
        let config = NodeConfig {
            alonzo_genesis_hash: Some(
                "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz".to_string(),
            ),
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(err.to_string().contains("Alonzo genesis hash"));
    }

    #[test]
    fn test_validate_valid_genesis_hash() {
        let config = NodeConfig {
            shelley_genesis_hash: Some(
                "363498d1024f84bb39d3fa9593ce391483cb40d479b87233f868d6e57c3a400d".to_string(),
            ),
            ..NodeConfig::default()
        };
        assert!(config.validate(Path::new(".")).is_ok());
    }

    #[test]
    fn test_dijkstra_genesis_fields_deserialise() {
        // Mirrors cardano-node's PascalCase field names for the new
        // post-Conway HFC genesis (issue #462 Phase 6 / Phase 4).
        let json = r#"{
            "DijkstraGenesisFile": "preview-dijkstra-genesis.json",
            "DijkstraGenesisHash": "0000000000000000000000000000000000000000000000000000000000000000"
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.dijkstra_genesis_file.as_deref(),
            Some("preview-dijkstra-genesis.json")
        );
        assert_eq!(
            config.dijkstra_genesis_hash.as_deref(),
            Some("0000000000000000000000000000000000000000000000000000000000000000")
        );
    }

    #[test]
    fn test_validate_invalid_dijkstra_genesis_hash() {
        let config = NodeConfig {
            dijkstra_genesis_hash: Some("deadbeef".to_string()),
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(
            err.to_string().contains("Dijkstra genesis hash"),
            "validator must mention Dijkstra by name; got: {err}"
        );
    }

    #[test]
    fn test_validate_missing_dijkstra_genesis_file() {
        let config = NodeConfig {
            dijkstra_genesis_file: Some("nonexistent-dijkstra-genesis.json".to_string()),
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(
            err.to_string().contains("Dijkstra genesis file not found"),
            "expected Dijkstra file-not-found error; got: {err}"
        );
    }

    // ── MetricsPort config field ──────────────────────────────────────────────

    #[test]
    fn test_default_config_has_no_metrics_port() {
        // When the field is absent from the config file the operator gets None,
        // and the node binary falls back to 12798 (matching cardano-node's default).
        let config = NodeConfig::default();
        assert!(config.metrics_port.is_none());
    }

    #[test]
    fn test_metrics_port_from_json() {
        // Verify that "MetricsPort" is correctly deserialised from config JSON.
        let json = r#"{"MetricsPort": 9876}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.metrics_port, Some(9876));
    }

    #[test]
    fn test_metrics_port_zero_from_json() {
        // Port 0 in the config file should disable metrics (same semantics as
        // the --metrics-port 0 CLI flag).
        let json = r#"{"MetricsPort": 0}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.metrics_port, Some(0));
    }

    #[test]
    fn test_metrics_port_absent_from_json() {
        // Absence of the field must deserialise as None so the node can fall
        // through to the default port.
        let json = r#"{}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert!(config.metrics_port.is_none());
    }

    #[test]
    fn test_metrics_port_round_trip_serialise() {
        // Confirm that a port value survives a JSON round-trip.
        let original = NodeConfig {
            metrics_port: Some(8080),
            ..NodeConfig::default()
        };
        let json = serde_json::to_string(&original).unwrap();
        let restored: NodeConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.metrics_port, Some(8080));
    }

    // ── Metrics port resolution priority ─────────────────────────────────────
    //
    // The node binary resolves the effective port with this priority:
    //   1. --no-metrics  → 0
    //   2. --metrics-port → explicit CLI value
    //   3. config MetricsPort → site-wide default from file
    //   4. 12798 (matches cardano-node's default)
    //
    // We test the rule table here using plain functions that mirror the
    // logic in run_node() so the tests stay fast and do not require spawning
    // an actual server.

    const DUGITE_DEFAULT_METRICS_PORT: u16 = 12798;

    fn resolve_metrics_port(no_metrics: bool, cli: Option<u16>, config: Option<u16>) -> u16 {
        if no_metrics {
            0
        } else if let Some(p) = cli {
            p
        } else {
            config.unwrap_or(DUGITE_DEFAULT_METRICS_PORT)
        }
    }

    #[test]
    fn test_resolve_no_metrics_flag_wins_over_all() {
        // --no-metrics must win even when a CLI port and a config port are set.
        assert_eq!(resolve_metrics_port(true, Some(9000), Some(8000)), 0);
    }

    #[test]
    fn test_resolve_cli_port_wins_over_config() {
        assert_eq!(resolve_metrics_port(false, Some(9000), Some(8000)), 9000);
    }

    #[test]
    fn test_resolve_config_port_used_when_no_cli() {
        assert_eq!(resolve_metrics_port(false, None, Some(8080)), 8080);
    }

    #[test]
    fn test_resolve_falls_back_to_default_12798() {
        assert_eq!(resolve_metrics_port(false, None, None), 12798);
    }

    #[test]
    fn test_resolve_cli_port_zero_disables_metrics() {
        // Passing --metrics-port 0 from the CLI should disable the server.
        assert_eq!(resolve_metrics_port(false, Some(0), None), 0);
    }

    #[test]
    fn test_resolve_config_port_zero_disables_metrics() {
        // Setting MetricsPort=0 in the config file should also disable the server.
        assert_eq!(resolve_metrics_port(false, None, Some(0)), 0);
    }

    // ── RPC config resolution (#672 M1.A) ────────────────────────────────────

    fn empty_rpc_json(enabled: bool) -> RpcConfigJson {
        RpcConfigJson {
            enabled,
            listen_addr: None,
            port: None,
            max_concurrent_streams: None,
            stream_buffer_size: None,
            reflection_enabled: None,
            web_enabled: None,
            alpha_enabled: None,
            tls: None,
        }
    }

    #[test]
    fn rpc_resolve_no_rpc_flag_wins_over_all() {
        let cfg = empty_rpc_json(true);
        let out = resolve_rpc(true, Some("0.0.0.0"), Some(9999), Some(&cfg)).expect("resolve");
        assert!(out.is_none(), "--no-rpc must disable regardless");
    }

    #[test]
    fn rpc_resolve_returns_none_when_disabled_and_no_cli() {
        let cfg = empty_rpc_json(false);
        let out = resolve_rpc(false, None, None, Some(&cfg)).expect("resolve");
        assert!(out.is_none());
        let out2 = resolve_rpc(false, None, None, None).expect("resolve");
        assert!(out2.is_none());
    }

    #[test]
    fn rpc_resolve_cli_port_alone_enables_server() {
        let out = resolve_rpc(false, None, Some(7777), None)
            .expect("resolve")
            .expect("Some(config)");
        assert_eq!(out.port, 7777);
        assert_eq!(out.bind, std::net::IpAddr::from([127, 0, 0, 1]));
    }

    #[test]
    fn rpc_resolve_cli_host_alone_enables_server() {
        let out = resolve_rpc(false, Some("0.0.0.0"), None, None)
            .expect("resolve")
            .expect("Some(config)");
        assert_eq!(out.port, dugite_rpc::config::DEFAULT_RPC_PORT);
        assert_eq!(out.bind, std::net::IpAddr::from([0, 0, 0, 0]));
    }

    #[test]
    fn rpc_resolve_cli_overrides_config() {
        let mut cfg = empty_rpc_json(true);
        cfg.port = Some(1000);
        cfg.listen_addr = Some("10.0.0.1".into());
        let out = resolve_rpc(false, Some("0.0.0.0"), Some(2000), Some(&cfg))
            .expect("resolve")
            .expect("Some(config)");
        assert_eq!(out.port, 2000);
        assert_eq!(out.bind, std::net::IpAddr::from([0, 0, 0, 0]));
    }

    #[test]
    fn rpc_resolve_config_only_uses_config_values() {
        let mut cfg = empty_rpc_json(true);
        cfg.port = Some(54321);
        cfg.listen_addr = Some("192.168.1.10".into());
        cfg.web_enabled = Some(true);
        cfg.alpha_enabled = Some(false);
        let out = resolve_rpc(false, None, None, Some(&cfg))
            .expect("resolve")
            .expect("Some(config)");
        assert_eq!(out.port, 54321);
        assert_eq!(out.bind, std::net::IpAddr::from([192, 168, 1, 10]));
        assert!(out.web_enabled);
        assert!(!out.alpha_enabled);
    }

    #[test]
    fn rpc_resolve_rejects_malformed_addr() {
        let mut cfg = empty_rpc_json(true);
        cfg.listen_addr = Some("not-an-ip".into());
        let err = resolve_rpc(false, None, None, Some(&cfg)).unwrap_err();
        assert!(err.to_lowercase().contains("rpc.listenaddr") || err.contains("IP"));
    }

    #[test]
    fn rpc_resolve_rejects_malformed_cli_host() {
        let err = resolve_rpc(false, Some("not-an-ip"), Some(1), None).unwrap_err();
        assert!(err.to_lowercase().contains("rpc-host") || err.contains("IP"));
    }

    // ── existing metrics tests follow ────────────────────────────────────────

    #[test]
    fn test_default_metrics_port_matches_cardano_node() {
        // Dugite defaults to 12798, matching cardano-node. When co-locating
        // multiple nodes, operators must assign distinct ports via CLI flags.
        assert_eq!(DUGITE_DEFAULT_METRICS_PORT, 12798);
    }

    // ── DiffusionMode config field ──────────────────────────────────────────

    #[test]
    fn test_default_diffusion_mode() {
        let config = NodeConfig::default();
        assert_eq!(config.diffusion_mode, DiffusionMode::InitiatorAndResponder);
    }

    #[test]
    fn test_diffusion_mode_initiator_only_from_json() {
        let json = r#"{"DiffusionMode": "InitiatorOnly"}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.diffusion_mode, DiffusionMode::InitiatorOnly);
    }

    #[test]
    fn test_diffusion_mode_initiator_and_responder_from_json() {
        let json = r#"{"DiffusionMode": "InitiatorAndResponder"}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.diffusion_mode, DiffusionMode::InitiatorAndResponder);
    }

    #[test]
    fn test_diffusion_mode_absent_defaults_to_initiator_and_responder() {
        let json = r#"{}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.diffusion_mode, DiffusionMode::InitiatorAndResponder);
    }

    #[test]
    fn test_diffusion_mode_display() {
        assert_eq!(DiffusionMode::InitiatorOnly.to_string(), "InitiatorOnly");
        assert_eq!(
            DiffusionMode::InitiatorAndResponder.to_string(),
            "InitiatorAndResponder"
        );
    }

    // ── PeerSharing config field ────────────────────────────────────────────

    #[test]
    fn test_default_peer_sharing_is_none() {
        let config = NodeConfig::default();
        assert!(config.peer_sharing.is_none());
    }

    #[test]
    fn test_peer_sharing_true_from_json() {
        let json = r#"{"PeerSharing": true}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.peer_sharing, Some(true));
    }

    #[test]
    fn test_peer_sharing_false_from_json() {
        let json = r#"{"PeerSharing": false}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.peer_sharing, Some(false));
    }

    #[test]
    fn test_effective_peer_sharing_auto_relay() {
        // Relay (not BP) with no explicit setting → enabled
        let config = NodeConfig::default();
        assert!(config.effective_peer_sharing(false));
    }

    #[test]
    fn test_effective_peer_sharing_auto_block_producer() {
        // Block producer with no explicit setting → disabled
        let config = NodeConfig::default();
        assert!(!config.effective_peer_sharing(true));
    }

    #[test]
    fn test_effective_peer_sharing_explicit_override() {
        // Explicit true overrides BP auto-disable
        let config = NodeConfig {
            peer_sharing: Some(true),
            ..NodeConfig::default()
        };
        assert!(config.effective_peer_sharing(true));
    }

    // ── ConsensusMode config field ──────────────────────────────────────────

    #[test]
    fn test_consensus_mode_default() {
        let config = NodeConfig::default();
        assert_eq!(config.consensus_mode, ConsensusMode::PraosMode);
    }

    #[test]
    fn test_consensus_mode_genesis_from_json() {
        let json = r#"{"ConsensusMode": "GenesisMode"}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.consensus_mode, ConsensusMode::GenesisMode);
    }

    #[test]
    fn test_consensus_mode_cardano_node_canonical_values() {
        // cardano-node 11.0.1 `NodeConsensusMode` accepts EXACTLY "Genesis" /
        // "Praos" (Cardano.Node.Types). A cardano-node config file must work
        // verbatim with dugite.
        let config: NodeConfig = serde_json::from_str(r#"{"ConsensusMode": "Genesis"}"#).unwrap();
        assert_eq!(config.consensus_mode, ConsensusMode::GenesisMode);
        let config: NodeConfig = serde_json::from_str(r#"{"ConsensusMode": "Praos"}"#).unwrap();
        assert_eq!(config.consensus_mode, ConsensusMode::PraosMode);
        // Legacy dugite spellings remain accepted as aliases.
        let config: NodeConfig = serde_json::from_str(r#"{"ConsensusMode": "PraosMode"}"#).unwrap();
        assert_eq!(config.consensus_mode, ConsensusMode::PraosMode);
    }

    #[test]
    fn test_consensus_mode_serializes_to_cardano_node_value() {
        // Round-trip emits the cardano-node canonical strings.
        assert_eq!(
            serde_json::to_string(&ConsensusMode::GenesisMode).unwrap(),
            r#""Genesis""#
        );
        assert_eq!(
            serde_json::to_string(&ConsensusMode::PraosMode).unwrap(),
            r#""Praos""#
        );
    }

    // ── LowLevelGenesisOptions (cardano-node GenesisConfigFlags) ────────────

    #[test]
    fn test_low_level_genesis_options_absent_yields_defaults() {
        // cardano-node `defaultGenesisConfigFlags`: all subsystems enabled,
        // every tunable at its upstream default.
        let config: NodeConfig = serde_json::from_str("{}").unwrap();
        let opts = config.low_level_genesis_options.unwrap_or_default();
        assert!(opts.enable_csj);
        assert!(opts.enable_loe_and_gdd);
        assert!(opts.enable_lop);
        assert_eq!(opts.effective_block_fetch_grace_period_secs(), 10.0);
        assert_eq!(opts.effective_bucket_capacity(), 100_000);
        assert_eq!(opts.effective_bucket_rate(), 500);
        assert_eq!(opts.effective_csj_jump_size(), 4320);
        assert_eq!(opts.effective_gdd_rate_limit_secs(), 1.0);
    }

    #[test]
    fn test_low_level_genesis_options_cardano_node_field_names() {
        // Field names from cardano-node 11.0.1 Cardano.Node.Orphans
        // (FromJSON GenesisConfigFlags): EnableCSJ, EnableLoEAndGDD, EnableLoP,
        // BlockFetchGracePeriod, BucketCapacity, BucketRate, CSJJumpSize,
        // GDDRateLimit — all optional.
        let json = r#"{
            "ConsensusMode": "Genesis",
            "LowLevelGenesisOptions": {
                "EnableCSJ": false,
                "EnableLoP": true,
                "BlockFetchGracePeriod": 22.5,
                "BucketCapacity": 50000,
                "CSJJumpSize": 1000
            }
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        let opts = config.low_level_genesis_options.unwrap();
        assert!(!opts.enable_csj);
        assert!(opts.enable_loe_and_gdd); // absent → default true
        assert!(opts.enable_lop);
        assert_eq!(opts.effective_block_fetch_grace_period_secs(), 22.5);
        assert_eq!(opts.effective_bucket_capacity(), 50_000);
        assert_eq!(opts.effective_bucket_rate(), 500); // absent → default
        assert_eq!(opts.effective_csj_jump_size(), 1000);
        assert_eq!(opts.effective_gdd_rate_limit_secs(), 1.0); // absent → default
    }

    // ── ConsensusMode CLI/JSON resolution (#535) ─────────────────────────────

    #[test]
    fn test_resolve_consensus_mode_cli_overrides_config() {
        // CLI explicitly says praos → wins over config GenesisMode.
        assert_eq!(
            resolve_consensus_mode(Some("praos"), ConsensusMode::GenesisMode),
            ("praos", "cli")
        );
        // CLI explicitly says genesis → wins over config PraosMode.
        assert_eq!(
            resolve_consensus_mode(Some("genesis"), ConsensusMode::PraosMode),
            ("genesis", "cli")
        );
    }

    #[test]
    fn test_resolve_consensus_mode_config_used_when_cli_absent() {
        // No CLI flag → JSON config GenesisMode is honoured (#535 main bug).
        assert_eq!(
            resolve_consensus_mode(None, ConsensusMode::GenesisMode),
            ("genesis", "config")
        );
        // No CLI flag, config PraosMode → praos.
        assert_eq!(
            resolve_consensus_mode(None, ConsensusMode::PraosMode),
            ("praos", "config")
        );
    }

    #[test]
    fn test_resolve_consensus_mode_unexpected_cli_falls_back_to_config() {
        // clap normally rejects unknown values via `value_parser`, but the
        // helper must be total — if a future caller bypasses clap we fall
        // back to the JSON source-of-truth rather than panic.
        assert_eq!(
            resolve_consensus_mode(Some("weird"), ConsensusMode::GenesisMode),
            ("genesis", "config")
        );
    }

    #[test]
    fn test_consensus_mode_as_runtime_str() {
        assert_eq!(ConsensusMode::PraosMode.as_runtime_str(), "praos");
        assert_eq!(ConsensusMode::GenesisMode.as_runtime_str(), "genesis");
    }

    // ── Genesis sync targets ────────────────────────────────────────────────

    #[test]
    fn test_sync_targets_from_json() {
        let json = r#"{
            "SyncTargetNumberOfActiveBigLedgerPeers": 25,
            "MinBigLedgerPeersForTrustedState": 10
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.sync_target_number_of_active_big_ledger_peers, 25);
        assert_eq!(config.min_big_ledger_peers_for_trusted_state, 10);
    }

    // ── AcceptedConnectionsLimit ────────────────────────────────────────────

    #[test]
    fn test_accepted_connections_limit_from_json() {
        // Short keys matching Haskell's hand-written FromJSON instance.
        let json = r#"{
            "AcceptedConnectionsLimit": {
                "hardLimit": 256,
                "softLimit": 200,
                "delay": 2.0
            }
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        let limit = config.accepted_connections_limit.unwrap();
        assert_eq!(limit.hard_limit, 256);
        assert_eq!(limit.soft_limit, 200);
        assert!((limit.delay - 2.0_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn test_accepted_connections_limit_backward_compat_aliases() {
        // Old long camelCase keys must still parse via serde aliases.
        let json = r#"{
            "AcceptedConnectionsLimit": {
                "acceptedConnectionsHardLimit": 300,
                "acceptedConnectionsSoftLimit": 250,
                "acceptedConnectionsDelay": 3
            }
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        let limit = config.accepted_connections_limit.unwrap();
        assert_eq!(limit.hard_limit, 300);
        assert_eq!(limit.soft_limit, 250);
        assert!((limit.delay - 3.0_f64).abs() < f64::EPSILON);
    }

    // ── Connection timeouts ─────────────────────────────────────────────────

    #[test]
    fn test_connection_timeouts_from_json() {
        let json = r#"{
            "ProtocolIdleTimeout": 10,
            "TimeWaitTimeout": 120,
            "EgressPollInterval": 20
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert!((config.protocol_idle_timeout - 10.0_f64).abs() < f64::EPSILON);
        assert!((config.time_wait_timeout - 120.0_f64).abs() < f64::EPSILON);
        assert!((config.egress_poll_interval - 20.0_f64).abs() < f64::EPSILON);
    }

    #[test]
    fn test_connection_timeouts_fractional() {
        // Fractional seconds must parse correctly — Haskell uses DiffTime.
        let json = r#"{
            "ProtocolIdleTimeout": 5.5,
            "TimeWaitTimeout": 60.25,
            "EgressPollInterval": 0.1,
            "ChainSyncIdleTimeout": 3373.5
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert!((config.protocol_idle_timeout - 5.5_f64).abs() < f64::EPSILON);
        assert!((config.time_wait_timeout - 60.25_f64).abs() < f64::EPSILON);
        assert!((config.egress_poll_interval - 0.1_f64).abs() < f64::EPSILON);
        assert!((config.chain_sync_idle_timeout.unwrap() - 3373.5_f64).abs() < f64::EPSILON);
    }

    // ── All new fields absent → defaults ────────────────────────────────────

    #[test]
    fn test_new_config_fields_absent_use_defaults() {
        let json = r#"{}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.consensus_mode, ConsensusMode::PraosMode);
        assert_eq!(config.sync_target_number_of_active_big_ledger_peers, 30);
        assert_eq!(
            config.sync_target_number_of_established_big_ledger_peers,
            40
        );
        assert_eq!(config.sync_target_number_of_known_big_ledger_peers, 100);
        assert_eq!(config.min_big_ledger_peers_for_trusted_state, 5);
        assert!((config.protocol_idle_timeout - 5.0_f64).abs() < f64::EPSILON);
        assert!((config.time_wait_timeout - 60.0_f64).abs() < f64::EPSILON);
        assert!((config.egress_poll_interval - 0.0_f64).abs() < f64::EPSILON);
        assert!(config.accepted_connections_limit.is_none());
        assert!(config.chain_sync_idle_timeout.is_none());
    }

    // ── Peer target ordering validation tests ─────────────────────────

    #[test]
    fn test_validate_known_less_than_established_fails() {
        let config = NodeConfig {
            target_number_of_known_peers: 10,
            target_number_of_established_peers: 20,
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        let msg = err.to_string();
        // New format: "[deadline] established (20) must be <= known (10)"
        assert!(
            msg.contains("[deadline]") && msg.contains("established"),
            "expected deadline/established in error, got: {msg}"
        );
    }

    #[test]
    fn test_validate_established_less_than_active_fails() {
        let config = NodeConfig {
            target_number_of_established_peers: 5,
            target_number_of_active_peers: 10,
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        let msg = err.to_string();
        // New format: "[deadline] active (10) must be <= established (5)"
        assert!(
            msg.contains("[deadline]") && msg.contains("active"),
            "expected deadline/active in error, got: {msg}"
        );
    }

    #[test]
    fn test_validate_blp_known_less_than_established_fails() {
        let config = NodeConfig {
            target_number_of_known_big_ledger_peers: 3,
            target_number_of_established_big_ledger_peers: 10,
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        let msg = err.to_string();
        // New format: "[deadline] established_big_ledger_peers (10) must be <= known_big_ledger_peers (3)"
        assert!(
            msg.contains("[deadline]") && msg.contains("big_ledger_peers"),
            "expected deadline/big_ledger_peers in error, got: {msg}"
        );
    }

    #[test]
    fn test_validate_genesis_sync_targets() {
        // Invalid sync targets (established > known) must fail regardless of ConsensusMode.
        let config = NodeConfig {
            sync_target_number_of_known_peers: 5,
            sync_target_number_of_established_peers: 10,
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(
            err.to_string().contains("[sync]"),
            "expected [sync] label in error, got: {err}"
        );
    }

    #[test]
    fn test_validate_sync_targets_always_checked() {
        // Invalid sync targets must fail in PraosMode too (Haskell validates both
        // deadline and sync targets unconditionally, not just in GenesisMode).
        let config = NodeConfig {
            consensus_mode: ConsensusMode::PraosMode,
            sync_target_number_of_known_peers: 5,
            sync_target_number_of_established_peers: 10,
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(
            err.to_string().contains("[sync]"),
            "expected [sync] label in error, got: {err}"
        );
    }

    #[test]
    fn test_validate_root_exceeds_known_fails() {
        // root > known violates the third ordering invariant.
        let config = NodeConfig {
            target_number_of_root_peers: 200,
            target_number_of_known_peers: 150,
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(
            err.to_string().contains("root"),
            "expected root in error, got: {err}"
        );
    }

    #[test]
    fn test_validate_active_exceeds_100_fails() {
        // active > 100 violates the upper-bound safety limit.
        let config = NodeConfig {
            target_number_of_active_peers: 101,
            target_number_of_established_peers: 200,
            target_number_of_known_peers: 500,
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(
            err.to_string().contains("active") && err.to_string().contains("100"),
            "expected active/100 in error, got: {err}"
        );
    }

    #[test]
    fn test_validate_blp_upper_bounds() {
        // established_big_ledger_peers > 1000 violates the upper bound.
        let config = NodeConfig {
            target_number_of_active_big_ledger_peers: 5,
            target_number_of_established_big_ledger_peers: 1001,
            target_number_of_known_big_ledger_peers: 5000,
            ..NodeConfig::default()
        };
        let err = config.validate(Path::new(".")).unwrap_err();
        assert!(
            err.to_string().contains("big_ledger_peers") && err.to_string().contains("1000"),
            "expected big_ledger_peers/1000 in error, got: {err}"
        );
    }
}
