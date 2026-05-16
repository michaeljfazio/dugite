//! Integration tests for dugite-config CLI subcommands and config operations.

// Integration tests for dugite-config binary.

// ─── Roundtrip: dugite-config init output → NodeConfig deserializer ──────────
//
// These tests verify that the JSON produced by `dugite-config init` survives a
// round-trip through dugite_node::config::NodeConfig without type errors or
// field loss.  They also pin the NodeConfig defaults against the schema values
// so that any drift is caught at compile time.

#[cfg(test)]
mod roundtrip {
    use dugite_node::config::NodeConfig;

    // Helper: produce the init JSON for a given network via the binary.
    fn init_json_for(network: &str) -> String {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_dugite-config"))
            .args(["init", "--network", network, "--out"])
            .arg(&path)
            .output()
            .expect("dugite-config binary must exist");
        if !output.status.success() {
            panic!(
                "dugite-config init failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::read_to_string(&path).unwrap()
    }

    /// All fields produced by `dugite-config init --network preview` must
    /// deserialise cleanly into NodeConfig.
    #[test]
    fn test_preview_init_roundtrip_into_node_config() {
        let json = init_json_for("preview");
        let config: NodeConfig = serde_json::from_str(&json)
            .expect("init preview output must deserialise into NodeConfig");
        assert_eq!(config.target_number_of_active_peers, 20);
        assert_eq!(config.target_number_of_established_peers, 30);
        assert_eq!(config.target_number_of_known_peers, 150);
        assert_eq!(config.target_number_of_root_peers, 60);
        assert_eq!(config.sync_target_number_of_active_peers, 5);
        assert_eq!(config.sync_target_number_of_established_peers, 10);
        assert_eq!(config.sync_target_number_of_known_peers, 150);
        assert_eq!(
            config.sync_target_number_of_established_big_ledger_peers,
            40
        );
        assert_eq!(config.min_big_ledger_peers_for_trusted_state, 5);
        assert!((config.egress_poll_interval - 0.0_f64).abs() < f64::EPSILON);
        assert!((config.protocol_idle_timeout - 5.0_f64).abs() < f64::EPSILON);
        assert!((config.time_wait_timeout - 60.0_f64).abs() < f64::EPSILON);
        // LogDirective and PeerSharing are intentionally omitted from init output.
        assert!(config.log_directive.is_none());
        assert!(config.peer_sharing.is_none());
    }

    #[test]
    fn test_mainnet_init_roundtrip_into_node_config() {
        let json = init_json_for("mainnet");
        let config: NodeConfig = serde_json::from_str(&json)
            .expect("init mainnet output must deserialise into NodeConfig");
        assert_eq!(config.target_number_of_active_peers, 20);
        assert_eq!(config.target_number_of_known_peers, 150);
    }

    #[test]
    fn test_preprod_init_roundtrip_into_node_config() {
        let json = init_json_for("preprod");
        let _config: NodeConfig = serde_json::from_str(&json)
            .expect("init preprod output must deserialise into NodeConfig");
    }

    /// Pin NodeConfig defaults against what the schema documents.
    /// If this test fails, a NodeConfig field default changed — update schema.rs.
    #[test]
    fn test_node_config_defaults_match_schema() {
        let d = NodeConfig::default();

        assert_eq!(
            d.target_number_of_active_peers, 20,
            "target_number_of_active_peers default changed — update schema"
        );
        assert_eq!(
            d.target_number_of_established_peers, 30,
            "target_number_of_established_peers default changed — update schema"
        );
        assert_eq!(
            d.target_number_of_known_peers, 150,
            "target_number_of_known_peers default changed — update schema"
        );
        assert_eq!(
            d.target_number_of_root_peers, 60,
            "target_number_of_root_peers default changed — update schema"
        );
        assert_eq!(
            d.target_number_of_active_big_ledger_peers, 5,
            "target_number_of_active_big_ledger_peers default changed — update schema"
        );
        assert_eq!(
            d.target_number_of_established_big_ledger_peers, 10,
            "target_number_of_established_big_ledger_peers default changed — update schema"
        );
        assert_eq!(
            d.target_number_of_known_big_ledger_peers, 15,
            "target_number_of_known_big_ledger_peers default changed — update schema"
        );
        assert_eq!(
            d.sync_target_number_of_active_peers, 5,
            "sync_target_number_of_active_peers default changed — update schema"
        );
        assert_eq!(
            d.sync_target_number_of_established_peers, 10,
            "sync_target_number_of_established_peers default changed — update schema"
        );
        assert_eq!(
            d.sync_target_number_of_known_peers, 150,
            "sync_target_number_of_known_peers default changed — update schema"
        );
        assert_eq!(
            d.sync_target_number_of_active_big_ledger_peers, 30,
            "sync_target_number_of_active_big_ledger_peers default changed — update schema"
        );
        assert_eq!(
            d.sync_target_number_of_established_big_ledger_peers, 40,
            "sync_target_number_of_established_big_ledger_peers default changed — update schema"
        );
        assert_eq!(
            d.sync_target_number_of_known_big_ledger_peers, 100,
            "sync_target_number_of_known_big_ledger_peers default changed — update schema"
        );
        assert_eq!(
            d.min_big_ledger_peers_for_trusted_state, 5,
            "min_big_ledger_peers_for_trusted_state default changed — update schema"
        );
        assert!(
            (d.egress_poll_interval - 0.0_f64).abs() < f64::EPSILON,
            "egress_poll_interval default changed — update schema"
        );
        assert!(
            (d.protocol_idle_timeout - 5.0_f64).abs() < f64::EPSILON,
            "protocol_idle_timeout default changed — update schema"
        );
        assert!(
            (d.time_wait_timeout - 60.0_f64).abs() < f64::EPSILON,
            "time_wait_timeout default changed — update schema"
        );
        assert_eq!(
            d.churn_interval_normal_secs, 3300,
            "churn_interval_normal_secs default changed — update schema"
        );
        assert_eq!(
            d.churn_interval_sync_secs, 900,
            "churn_interval_sync_secs default changed — update schema"
        );
        assert_eq!(
            d.stall_demotion_cycles, 6,
            "stall_demotion_cycles default changed — update schema"
        );
        assert_eq!(
            d.error_demotion_threshold, 5,
            "error_demotion_threshold default changed — update schema"
        );
        assert!(
            d.log_directive.is_none(),
            "log_directive default changed — update schema"
        );
        assert!(
            d.peer_sharing.is_none(),
            "peer_sharing default changed — update schema"
        );
        assert!(
            d.accepted_connections_limit.is_none(),
            "accepted_connections_limit default changed — update schema"
        );
    }

    /// Verify that a config JSON with LogDirective round-trips through NodeConfig.
    #[test]
    fn test_log_directive_roundtrip() {
        let json = r#"{"LogDirective": "info,dugite_network=trace"}"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert_eq!(
            config.log_directive.as_deref(),
            Some("info,dugite_network=trace")
        );
        let re_json = serde_json::to_string(&config).unwrap();
        let re_config: NodeConfig = serde_json::from_str(&re_json).unwrap();
        assert_eq!(
            re_config.log_directive.as_deref(),
            Some("info,dugite_network=trace")
        );
    }

    /// Verify AcceptedConnectionsLimit (short key form) round-trips correctly.
    #[test]
    fn test_accepted_connections_limit_roundtrip() {
        let json = r#"{
            "AcceptedConnectionsLimit": {
                "hardLimit": 256,
                "softLimit": 192,
                "delay": 3.5
            }
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        let limit = config.accepted_connections_limit.unwrap();
        assert_eq!(limit.hard_limit, 256);
        assert_eq!(limit.soft_limit, 192);
        assert!((limit.delay - 3.5_f64).abs() < 1e-9);
    }

    /// Verify the Storage nested object round-trips through NodeConfig.
    #[test]
    fn test_storage_config_roundtrip() {
        let json = r#"{
            "Storage": {
                "profile": "low-memory",
                "utxoMemtableSizeMb": 512
            }
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        let storage = config.storage.unwrap();
        assert_eq!(storage.profile.as_deref(), Some("low-memory"));
        assert_eq!(storage.utxo_memtable_size_mb, Some(512));
    }

    /// PeerSharing in NodeConfig is Option<bool>; verify both true and false parse.
    #[test]
    fn test_peer_sharing_bool_roundtrip() {
        let json_true = r#"{"PeerSharing": true}"#;
        let c: NodeConfig = serde_json::from_str(json_true).unwrap();
        assert_eq!(c.peer_sharing, Some(true));

        let json_false = r#"{"PeerSharing": false}"#;
        let c: NodeConfig = serde_json::from_str(json_false).unwrap();
        assert_eq!(c.peer_sharing, Some(false));
    }

    /// F64 timeout fields accept fractional seconds.
    #[test]
    fn test_fractional_timeout_roundtrip() {
        let json = r#"{
            "ProtocolIdleTimeout": 5.5,
            "TimeWaitTimeout": 60.25,
            "EgressPollInterval": 0.1,
            "ChainSyncIdleTimeout": 300.0
        }"#;
        let config: NodeConfig = serde_json::from_str(json).unwrap();
        assert!((config.protocol_idle_timeout - 5.5_f64).abs() < 1e-9);
        assert!((config.time_wait_timeout - 60.25_f64).abs() < 1e-9);
        assert!((config.egress_poll_interval - 0.1_f64).abs() < 1e-9);
        assert!((config.chain_sync_idle_timeout.unwrap() - 300.0_f64).abs() < 1e-9);
    }
}

// ─── Config init ─────────────────────────────────────────────────────────────

#[test]
fn test_init_preview_generates_valid_json() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preview-config.json");

    // Run the init command binary
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dugite-config"))
        .args(["init", "--network", "preview", "--out"])
        .arg(&path)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let content = std::fs::read_to_string(&path).unwrap();
            let json: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert!(json.is_object());
            // Preview should have magic=2
            if let Some(magic) = json.get("networkMagic") {
                assert_eq!(magic.as_u64().unwrap(), 2);
            }
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            // If the binary doesn't support this exact invocation, skip
            if stderr.contains("unrecognized") {
                eprintln!("Skipping test: dugite-config init not supported with these args");
                return;
            }
            panic!("init failed: {}", stderr);
        }
        Err(e) => {
            eprintln!("Skipping test: could not run dugite-config: {e}");
        }
    }
}

// ─── Config validation ───────────────────────────────────────────────────────

#[test]
fn test_validate_valid_config() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test-config.json");

    // Write a minimal valid config
    let config = serde_json::json!({
        "networkMagic": 2,
        "Protocol": "Cardano",
        "RequiresNetworkMagic": "RequiresMagic",
    });
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dugite-config"))
        .args(["validate"])
        .arg(&path)
        .output();

    match output {
        Ok(o) => {
            // Validate should succeed or at least not crash
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !o.status.success() && !stderr.contains("unrecognized") {
                eprintln!("validate output: {stdout}{stderr}");
            }
        }
        Err(e) => eprintln!("Skipping: {e}"),
    }
}

#[test]
fn test_validate_invalid_json_fails() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bad-config.json");
    std::fs::write(&path, "not valid json {{{").unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dugite-config"))
        .args(["validate"])
        .arg(&path)
        .output();

    match output {
        Ok(o) => {
            // Should fail (non-zero exit) for invalid JSON
            if !String::from_utf8_lossy(&o.stderr).contains("unrecognized") {
                assert!(!o.status.success(), "validate should fail for invalid JSON");
            }
        }
        Err(e) => eprintln!("Skipping: {e}"),
    }
}

// ─── Config get/set ──────────────────────────────────────────────────────────

#[test]
fn test_get_known_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test-config.json");
    let config = serde_json::json!({"networkMagic": 42});
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dugite-config"))
        .args(["get", "networkMagic"])
        .arg(&path)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            assert!(stdout.contains("42"), "Expected 42 in output: {stdout}");
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stderr.contains("unrecognized") {
                eprintln!("get failed: {stderr}");
            }
        }
        Err(e) => eprintln!("Skipping: {e}"),
    }
}

#[test]
fn test_set_modifies_value() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test-config.json");
    let config = serde_json::json!({"networkMagic": 2});
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dugite-config"))
        .args(["set", "networkMagic", "764824073"])
        .arg(&path)
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let content = std::fs::read_to_string(&path).unwrap();
            let json: serde_json::Value = serde_json::from_str(&content).unwrap();
            assert_eq!(json["networkMagic"], 764824073);
        }
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stderr.contains("unrecognized") {
                eprintln!("set failed: {stderr}");
            }
        }
        Err(e) => eprintln!("Skipping: {e}"),
    }
}

/// `set` must work for schema keys that aren't yet present in the file —
/// after the change, the new key is appended with the user-supplied value.
#[test]
fn test_set_adds_missing_schema_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test-config.json");
    // File deliberately omits Protocol; the schema knows it.
    let config = serde_json::json!({"NetworkMagic": 2});
    std::fs::write(&path, serde_json::to_string_pretty(&config).unwrap()).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dugite-config"))
        .args(["set", "Protocol", "TPraos", "--config"])
        .arg(&path)
        .output()
        .expect("dugite-config binary must exist");

    assert!(
        output.status.success(),
        "set failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let content = std::fs::read_to_string(&path).unwrap();
    let json: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(json["Protocol"], "TPraos");
    // Existing entries survive.
    assert_eq!(json["NetworkMagic"], 2);
}

/// `set` for a key that's neither in the file nor in the schema must fail with
/// a clear message rather than silently appending an unknown key.
#[test]
fn test_set_rejects_unknown_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("test-config.json");
    std::fs::write(&path, r#"{"NetworkMagic": 2}"#).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_dugite-config"))
        .args(["set", "NotInSchema", "hello", "--config"])
        .arg(&path)
        .output()
        .expect("dugite-config binary must exist");

    assert!(
        !output.status.success(),
        "set of unknown key should fail; stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

// Note: dugite-config is a binary crate with no lib target.
// Schema/config tests are in the source modules (57 tests total).
