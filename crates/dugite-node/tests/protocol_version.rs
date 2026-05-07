use dugite_node::config::NodeConfig;
use dugite_node::forge::BlockProducerConfig;
use dugite_primitives::block::ProtocolVersion;

// ---------------------------------------------------------------------------
// NodeConfig → node_protocol_version()
// ---------------------------------------------------------------------------

/// Default config (no ExperimentalHardForksEnabled) should produce ProtVer 11,0.
/// Tracks current cardano-node mainnet (Conway, ProtVer 11).
#[test]
fn default_config_protocol_version_is_11_0() {
    let json = r#"{}"#;
    let config: NodeConfig = serde_json::from_str(json).unwrap();
    let pv = config.node_protocol_version();
    assert_eq!(pv.major, 11);
    assert_eq!(pv.minor, 0);
}

/// ExperimentalHardForksEnabled=false should produce ProtVer 11,0.
#[test]
fn experimental_false_protocol_version_is_11_0() {
    let json = r#"{"ExperimentalHardForksEnabled": false}"#;
    let config: NodeConfig = serde_json::from_str(json).unwrap();
    let pv = config.node_protocol_version();
    assert_eq!(pv.major, 11);
    assert_eq!(pv.minor, 0);
}

/// ExperimentalHardForksEnabled=true should produce ProtVer 12,0
/// (Dijkstra, active on preview from 2026-05-07).
#[test]
fn experimental_true_protocol_version_is_12_0() {
    let json = r#"{"ExperimentalHardForksEnabled": true}"#;
    let config: NodeConfig = serde_json::from_str(json).unwrap();
    let pv = config.node_protocol_version();
    assert_eq!(pv.major, 12);
    assert_eq!(pv.minor, 0);
}

// ---------------------------------------------------------------------------
// NodeConfig → max_major_protocol_version()
// ---------------------------------------------------------------------------

/// max_major_protocol_version() must equal node_protocol_version().major.
#[test]
fn max_major_protocol_version_matches_node_pv_major() {
    let default: NodeConfig = serde_json::from_str("{}").unwrap();
    assert_eq!(default.max_major_protocol_version(), 11);

    let experimental: NodeConfig =
        serde_json::from_str(r#"{"ExperimentalHardForksEnabled": true}"#).unwrap();
    assert_eq!(experimental.max_major_protocol_version(), 12);
}

// ---------------------------------------------------------------------------
// BlockProducerConfig default
// ---------------------------------------------------------------------------

/// Default BlockProducerConfig must match cardano-node mainnet Conway (ProtVer 11,0).
#[test]
fn default_block_producer_config_matches_cardano_node() {
    let config = BlockProducerConfig::default();
    assert_eq!(
        config.protocol_version,
        ProtocolVersion {
            major: 11,
            minor: 0
        },
        "Default BlockProducerConfig should match cardano-node Conway mainnet (ProtVer 11,0)"
    );
}

/// BlockProducerConfig accepts custom protocol version for experimental (Dijkstra) mode.
#[test]
fn block_producer_config_accepts_experimental_version() {
    let config = BlockProducerConfig {
        protocol_version: ProtocolVersion {
            major: 12,
            minor: 0,
        },
        ..Default::default()
    };
    assert_eq!(config.protocol_version.major, 12);
    assert_eq!(config.protocol_version.minor, 0);
}

// ---------------------------------------------------------------------------
// End-to-end: NodeConfig → BlockProducerConfig
// ---------------------------------------------------------------------------

/// Verify that NodeConfig.node_protocol_version() produces a value suitable
/// for BlockProducerConfig in both default and experimental modes.
#[test]
fn config_to_block_producer_config_end_to_end() {
    // Default mode (Conway mainnet)
    let node_config: NodeConfig = serde_json::from_str("{}").unwrap();
    let bp_config = BlockProducerConfig {
        protocol_version: node_config.node_protocol_version(),
        ..Default::default()
    };
    assert_eq!(
        bp_config.protocol_version,
        ProtocolVersion {
            major: 11,
            minor: 0
        }
    );

    // Experimental mode (Dijkstra, preview testnet from 2026-05-07)
    let node_config: NodeConfig =
        serde_json::from_str(r#"{"ExperimentalHardForksEnabled": true}"#).unwrap();
    let bp_config = BlockProducerConfig {
        protocol_version: node_config.node_protocol_version(),
        ..Default::default()
    };
    assert_eq!(
        bp_config.protocol_version,
        ProtocolVersion {
            major: 12,
            minor: 0
        }
    );
}
