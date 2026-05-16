//! Runtime config reload logic for SIGHUP-driven live config changes.
//!
//! # Overview
//!
//! When the operator edits the node config file and sends `SIGHUP`,
//! [`reload_partition`] computes which fields changed and classifies each
//! field as:
//!
//! - **Applied** — can be hot-reloaded without a process restart.
//! - **Ignored** — changed but not hot-reloadable; a restart is required.
//!
//! The live [`RuntimeConfig`] is then atomically updated via
//! [`tokio::sync::watch`] for the changed reloadable fields.
//!
//! # Hot-reloadable fields
//!
//! | Field                                        | Notes                     |
//! |----------------------------------------------|---------------------------|
//! | `target_number_of_active_peers`              | Peer governor deadline    |
//! | `target_number_of_established_peers`         | Peer governor deadline    |
//! | `target_number_of_known_peers`               | Peer governor deadline    |
//! | `target_number_of_root_peers`                | Peer governor deadline    |
//! | `target_number_of_active_big_ledger_peers`   | Peer governor deadline    |
//! | `target_number_of_established_big_ledger_peers` | Peer governor deadline |
//! | `target_number_of_known_big_ledger_peers`    | Peer governor deadline    |
//! | `log_directive` / `min_severity`             | Tracing filter (existing) |
//! | `churn_interval_normal_secs`                 | Peer governor churn       |
//! | `churn_interval_sync_secs`                   | Peer governor churn       |
//! | `stall_demotion_cycles`                      | Peer governor stall       |
//! | `error_demotion_threshold`                   | Peer governor errors      |
//!
//! # Restart-required fields
//!
//! All other fields (genesis paths, database/socket paths, network magic,
//! listen address/port, KES/VRF/OpCert paths, metrics port) require an
//! explicit restart. Changes to these fields are logged as warnings but
//! do not abort the reload — the reloadable fields are still applied.
//!
//! # `DUGITE_PIPELINE_DEPTH` / `ChainSync pipeline depth`
//!
//! Currently captured as an env var at process startup. A config field
//! will be wired in a follow-up; new ChainSync handshakes pick up the env
//! var value at their initiation time, so no live reload is possible
//! without a handshake teardown.

use crate::config::NodeConfig;

// ---------------------------------------------------------------------------
// RuntimeConfig — the hot-reloadable subset of NodeConfig
// ---------------------------------------------------------------------------

/// The subset of [`NodeConfig`] that can be changed without restarting
/// the node. All consumers read from a [`tokio::sync::watch`] receiver
/// so they see the updated value within their next iteration.
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeConfig {
    // ── Peer governor — deadline targets ────────────────────────────────────
    /// Target number of active (hot) peers.
    pub target_number_of_active_peers: usize,
    /// Target number of established (warm) peers.
    pub target_number_of_established_peers: usize,
    /// Target number of known (cold) peers.
    pub target_number_of_known_peers: usize,
    /// Target number of root peers.
    pub target_number_of_root_peers: usize,
    /// Target number of active big ledger peers.
    pub target_number_of_active_big_ledger_peers: usize,
    /// Target number of established big ledger peers.
    pub target_number_of_established_big_ledger_peers: usize,
    /// Target number of known big ledger peers.
    pub target_number_of_known_big_ledger_peers: usize,

    // ── Peer governor — churn & demotion ────────────────────────────────────
    /// Governor churn interval during normal (caught-up) operation (seconds).
    pub churn_interval_normal_secs: u64,
    /// Governor churn interval during syncing (seconds).
    pub churn_interval_sync_secs: u64,
    /// Consecutive zero-block cycles before a hot peer is demoted to warm.
    pub stall_demotion_cycles: u32,
    /// Accumulated failure threshold above which a hot peer is demoted.
    pub error_demotion_threshold: u32,

    // ── Logging ─────────────────────────────────────────────────────────────
    /// Optional `tracing_subscriber::EnvFilter` directive, applied on SIGHUP.
    pub log_directive: Option<String>,
    /// Minimum severity string (fallback when `log_directive` is absent).
    pub min_severity: String,
}

impl RuntimeConfig {
    /// Extract the hot-reloadable fields from a full [`NodeConfig`].
    pub fn from_node_config(cfg: &NodeConfig) -> Self {
        Self {
            target_number_of_active_peers: cfg.target_number_of_active_peers,
            target_number_of_established_peers: cfg.target_number_of_established_peers,
            target_number_of_known_peers: cfg.target_number_of_known_peers,
            target_number_of_root_peers: cfg.target_number_of_root_peers,
            target_number_of_active_big_ledger_peers: cfg.target_number_of_active_big_ledger_peers,
            target_number_of_established_big_ledger_peers: cfg
                .target_number_of_established_big_ledger_peers,
            target_number_of_known_big_ledger_peers: cfg.target_number_of_known_big_ledger_peers,
            churn_interval_normal_secs: cfg.churn_interval_normal_secs,
            churn_interval_sync_secs: cfg.churn_interval_sync_secs,
            stall_demotion_cycles: cfg.stall_demotion_cycles,
            error_demotion_threshold: cfg.error_demotion_threshold,
            log_directive: cfg.log_directive.clone(),
            min_severity: cfg.min_severity.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reload plan
// ---------------------------------------------------------------------------

/// The outcome of [`reload_partition`]: which fields were applied, which
/// were ignored because they require a restart.
#[derive(Debug, Default)]
pub struct ReloadPlan {
    /// Field names that changed and have been (or will be) applied live.
    pub applied: Vec<&'static str>,
    /// Field names that changed but require a process restart.
    pub ignored: Vec<&'static str>,
}

impl ReloadPlan {
    /// `true` when at least one reloadable field changed.
    pub fn has_applied(&self) -> bool {
        !self.applied.is_empty()
    }
}

/// Compare `old` and `new` [`NodeConfig`] values and return a [`ReloadPlan`]
/// describing which fields changed and how they should be handled.
///
/// This function is **pure** — it never reads from disk or sends signals.
/// The actual application of the plan is performed by the SIGHUP handler
/// in `node/mod.rs` after calling this function.
///
/// # Examples
///
/// ```
/// use dugite_node::config::NodeConfig;
/// use dugite_node::config_reload::reload_partition;
///
/// let old = NodeConfig::default();
/// let mut new = NodeConfig::default();
/// new.target_number_of_active_peers = 30;
///
/// let plan = reload_partition(&old, &new);
/// assert!(plan.applied.contains(&"target_number_of_active_peers"));
/// assert!(plan.ignored.is_empty());
/// ```
pub fn reload_partition(old: &NodeConfig, new: &NodeConfig) -> ReloadPlan {
    let mut plan = ReloadPlan::default();

    // ── Hot-reloadable fields ────────────────────────────────────────────────
    macro_rules! check_reloadable {
        ($field:ident) => {
            if old.$field != new.$field {
                plan.applied.push(stringify!($field));
            }
        };
    }

    check_reloadable!(target_number_of_active_peers);
    check_reloadable!(target_number_of_established_peers);
    check_reloadable!(target_number_of_known_peers);
    check_reloadable!(target_number_of_root_peers);
    check_reloadable!(target_number_of_active_big_ledger_peers);
    check_reloadable!(target_number_of_established_big_ledger_peers);
    check_reloadable!(target_number_of_known_big_ledger_peers);
    check_reloadable!(churn_interval_normal_secs);
    check_reloadable!(churn_interval_sync_secs);
    check_reloadable!(stall_demotion_cycles);
    check_reloadable!(error_demotion_threshold);
    check_reloadable!(log_directive);
    check_reloadable!(min_severity);

    // ── Restart-required fields ──────────────────────────────────────────────
    macro_rules! check_restart_required {
        ($field:ident) => {
            if old.$field != new.$field {
                plan.ignored.push(stringify!($field));
            }
        };
    }

    check_restart_required!(network);
    check_restart_required!(network_magic);
    check_restart_required!(shelley_genesis_file);
    check_restart_required!(byron_genesis_file);
    check_restart_required!(alonzo_genesis_file);
    check_restart_required!(conway_genesis_file);
    check_restart_required!(shelley_genesis_hash);
    check_restart_required!(byron_genesis_hash);
    check_restart_required!(alonzo_genesis_hash);
    check_restart_required!(conway_genesis_hash);
    check_restart_required!(metrics_port);
    check_restart_required!(diffusion_mode);
    check_restart_required!(experimental_hard_forks_enabled);
    check_restart_required!(consensus_mode);

    plan
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> NodeConfig {
        NodeConfig::default()
    }

    // ── reload_partition: reloadable fields ──────────────────────────────────

    #[test]
    fn test_partition_active_peers_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.target_number_of_active_peers = 30;

        let plan = reload_partition(&old, &new);
        assert!(
            plan.applied.contains(&"target_number_of_active_peers"),
            "applied = {:?}",
            plan.applied
        );
        assert!(plan.ignored.is_empty(), "ignored = {:?}", plan.ignored);
    }

    #[test]
    fn test_partition_established_peers_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.target_number_of_established_peers = 50;

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"target_number_of_established_peers"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_known_peers_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.target_number_of_known_peers = 200;

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"target_number_of_known_peers"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_root_peers_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.target_number_of_root_peers = 10;

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"target_number_of_root_peers"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_active_blp_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.target_number_of_active_big_ledger_peers = 10;

        let plan = reload_partition(&old, &new);
        assert!(plan
            .applied
            .contains(&"target_number_of_active_big_ledger_peers"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_established_blp_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.target_number_of_established_big_ledger_peers = 15;

        let plan = reload_partition(&old, &new);
        assert!(plan
            .applied
            .contains(&"target_number_of_established_big_ledger_peers"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_known_blp_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.target_number_of_known_big_ledger_peers = 20;

        let plan = reload_partition(&old, &new);
        assert!(plan
            .applied
            .contains(&"target_number_of_known_big_ledger_peers"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_churn_normal_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.churn_interval_normal_secs = 1800;

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"churn_interval_normal_secs"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_churn_sync_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.churn_interval_sync_secs = 450;

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"churn_interval_sync_secs"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_stall_demotion_cycles_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.stall_demotion_cycles = 10;

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"stall_demotion_cycles"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_error_demotion_threshold_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.error_demotion_threshold = 8;

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"error_demotion_threshold"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_log_directive_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.log_directive = Some("info,dugite_network=trace".to_string());

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"log_directive"));
        assert!(plan.ignored.is_empty());
    }

    #[test]
    fn test_partition_min_severity_is_applied() {
        let old = default_config();
        let mut new = default_config();
        new.min_severity = "Debug".to_string();

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"min_severity"));
        assert!(plan.ignored.is_empty());
    }

    // ── reload_partition: restart-required fields ────────────────────────────

    #[test]
    fn test_partition_network_magic_is_ignored() {
        let old = default_config();
        let mut new = default_config();
        new.network_magic = Some(2);

        let plan = reload_partition(&old, &new);
        assert!(
            plan.ignored.contains(&"network_magic"),
            "ignored = {:?}",
            plan.ignored
        );
        // Must NOT appear in applied
        assert!(
            !plan.applied.contains(&"network_magic"),
            "applied = {:?}",
            plan.applied
        );
    }

    #[test]
    fn test_partition_metrics_port_is_ignored() {
        let old = default_config();
        let mut new = default_config();
        new.metrics_port = Some(9999);

        let plan = reload_partition(&old, &new);
        assert!(plan.ignored.contains(&"metrics_port"));
    }

    #[test]
    fn test_partition_diffusion_mode_is_ignored() {
        let old = default_config();
        let mut new = default_config();
        new.diffusion_mode = crate::config::DiffusionMode::InitiatorOnly;

        let plan = reload_partition(&old, &new);
        assert!(plan.ignored.contains(&"diffusion_mode"));
    }

    #[test]
    fn test_partition_genesis_file_is_ignored() {
        let old = default_config();
        let mut new = default_config();
        new.shelley_genesis_file = Some("/new/path/shelley.json".to_string());

        let plan = reload_partition(&old, &new);
        assert!(plan.ignored.contains(&"shelley_genesis_file"));
    }

    // ── No-change case ───────────────────────────────────────────────────────

    #[test]
    fn test_partition_identical_configs_no_change() {
        let old = default_config();
        let new = default_config();

        let plan = reload_partition(&old, &new);
        assert!(
            plan.applied.is_empty(),
            "expected no applied changes, got {:?}",
            plan.applied
        );
        assert!(
            plan.ignored.is_empty(),
            "expected no ignored changes, got {:?}",
            plan.ignored
        );
        assert!(!plan.has_applied());
    }

    // ── Mixed changes ────────────────────────────────────────────────────────

    #[test]
    fn test_partition_mixed_reloadable_and_restart_required() {
        let old = default_config();
        let mut new = default_config();
        // Reloadable
        new.target_number_of_active_peers = 30;
        // Restart-required
        new.metrics_port = Some(9876);

        let plan = reload_partition(&old, &new);
        assert!(plan.applied.contains(&"target_number_of_active_peers"));
        assert!(plan.ignored.contains(&"metrics_port"));
    }

    // ── RuntimeConfig extraction ─────────────────────────────────────────────

    #[test]
    fn test_runtime_config_from_node_config_round_trip() {
        let cfg = default_config();
        let runtime = RuntimeConfig::from_node_config(&cfg);
        assert_eq!(
            runtime.target_number_of_active_peers,
            cfg.target_number_of_active_peers
        );
        assert_eq!(
            runtime.target_number_of_established_peers,
            cfg.target_number_of_established_peers
        );
        assert_eq!(
            runtime.target_number_of_known_peers,
            cfg.target_number_of_known_peers
        );
        assert_eq!(
            runtime.target_number_of_root_peers,
            cfg.target_number_of_root_peers
        );
        assert_eq!(
            runtime.target_number_of_active_big_ledger_peers,
            cfg.target_number_of_active_big_ledger_peers
        );
        assert_eq!(
            runtime.target_number_of_established_big_ledger_peers,
            cfg.target_number_of_established_big_ledger_peers
        );
        assert_eq!(
            runtime.target_number_of_known_big_ledger_peers,
            cfg.target_number_of_known_big_ledger_peers
        );
        assert_eq!(
            runtime.churn_interval_normal_secs,
            cfg.churn_interval_normal_secs
        );
        assert_eq!(
            runtime.churn_interval_sync_secs,
            cfg.churn_interval_sync_secs
        );
        assert_eq!(runtime.stall_demotion_cycles, cfg.stall_demotion_cycles);
        assert_eq!(
            runtime.error_demotion_threshold,
            cfg.error_demotion_threshold
        );
        assert_eq!(runtime.log_directive, cfg.log_directive);
        assert_eq!(runtime.min_severity, cfg.min_severity);
    }

    // ── NodeConfig round-trip serialization ──────────────────────────────────
    //
    // Regression guard: if someone adds a new field to NodeConfig without
    // updating reload_partition, the round-trip test still passes, but the
    // partition test for the NEW field will be missing.  This test catches the
    // simpler "field added but forgot serde attribute" class of bugs.
    #[test]
    fn test_node_config_json_roundtrip_preserves_all_fields() {
        let cfg = NodeConfig {
            network_magic: Some(764824073),
            target_number_of_active_peers: 25,
            target_number_of_established_peers: 40,
            target_number_of_known_peers: 200,
            target_number_of_root_peers: 5,
            target_number_of_active_big_ledger_peers: 8,
            target_number_of_established_big_ledger_peers: 12,
            target_number_of_known_big_ledger_peers: 18,
            churn_interval_normal_secs: 1800,
            churn_interval_sync_secs: 600,
            stall_demotion_cycles: 8,
            error_demotion_threshold: 3,
            log_directive: Some("info,dugite_network=debug".to_string()),
            min_severity: "Debug".to_string(),
            ..NodeConfig::default()
        };

        let json = serde_json::to_string(&cfg).expect("serialise must succeed");
        let restored: NodeConfig = serde_json::from_str(&json).expect("deserialise must succeed");

        // Spot-check all reload-relevant fields survived the round trip.
        assert_eq!(
            restored.target_number_of_active_peers,
            cfg.target_number_of_active_peers
        );
        assert_eq!(
            restored.target_number_of_established_peers,
            cfg.target_number_of_established_peers
        );
        assert_eq!(
            restored.target_number_of_known_peers,
            cfg.target_number_of_known_peers
        );
        assert_eq!(
            restored.target_number_of_active_big_ledger_peers,
            cfg.target_number_of_active_big_ledger_peers
        );
        assert_eq!(
            restored.target_number_of_established_big_ledger_peers,
            cfg.target_number_of_established_big_ledger_peers
        );
        assert_eq!(
            restored.target_number_of_known_big_ledger_peers,
            cfg.target_number_of_known_big_ledger_peers
        );
        assert_eq!(
            restored.churn_interval_normal_secs,
            cfg.churn_interval_normal_secs
        );
        assert_eq!(
            restored.churn_interval_sync_secs,
            cfg.churn_interval_sync_secs
        );
        assert_eq!(restored.stall_demotion_cycles, cfg.stall_demotion_cycles);
        assert_eq!(
            restored.error_demotion_threshold,
            cfg.error_demotion_threshold
        );
        assert_eq!(restored.log_directive, cfg.log_directive);
        assert_eq!(restored.min_severity, cfg.min_severity);
        assert_eq!(restored.network_magic, cfg.network_magic);
    }
}
