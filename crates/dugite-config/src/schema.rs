//! Parameter schema — known Cardano node configuration parameters.
//!
//! Each [`ParamDef`] describes a single JSON key that may appear in a Cardano
//! node configuration file: its display name, the logical section it belongs
//! to, its value type (with optional constraints), a human-readable
//! description, its documented default value, and an operator tuning hint.
//!
//! Unknown keys found in the loaded JSON file are displayed as raw JSON values
//! (editable as strings) and reported under the [`SECTION_UNKNOWN`] sentinel.
//!
//! # Sections
//!
//! Parameters are grouped into logical sections for the left-panel tree:
//!
//! | Section  | Contents                                                  |
//! |----------|-----------------------------------------------------------|
//! | Network  | P2P flags, peer targets, network magic                    |
//! | Genesis  | Paths and hashes for all four genesis files               |
//! | Protocol | Protocol name, Cardano mode, HFC flags                    |
//! | Logging  | Minimum severity, tracers, log format                     |
//! | Advanced | Performance knobs, memory limits, etc.                    |

use std::collections::HashMap;

use serde_json::Value;

// ---------------------------------------------------------------------------
// Section identifiers
// ---------------------------------------------------------------------------

/// Section name for parameters that have no known definition.
pub const SECTION_UNKNOWN: &str = "Unknown";

// ---------------------------------------------------------------------------
// Value type
// ---------------------------------------------------------------------------

/// The type of a configuration parameter's value.
///
/// Used both to validate user edits and to drive the appropriate in-place
/// editor widget (toggle, free-form text input, or enum cycling).
#[derive(Debug, Clone, PartialEq)]
pub enum ParamType {
    /// A JSON boolean (`true` / `false`).
    Bool,
    /// An unsigned integer in the range `[min, max]`.
    U64 { min: u64, max: u64 },
    /// A floating-point number (accepts integer or decimal input).
    F64 { min: f64, max: f64 },
    /// A free-form UTF-8 string (no validation beyond non-empty).
    String,
    /// One of a fixed set of string values (cycled with arrow keys).
    Enum { values: &'static [&'static str] },
    /// A file-system path (stored as a JSON string, shown with a path icon).
    Path,
    /// A nested JSON object with a fixed sub-schema. Empty `fields` means the
    /// schema does not yet model any sub-key (object is treated as opaque and
    /// edited as a single read-only row, same as before this feature).
    Object { fields: &'static [SubParamDef] },
}

impl ParamType {
    /// Return a short display label for the type (shown in the description panel).
    pub fn label(&self) -> &'static str {
        match self {
            ParamType::Bool => "bool",
            ParamType::U64 { .. } => "u64",
            ParamType::F64 { .. } => "f64",
            ParamType::String => "string",
            ParamType::Enum { .. } => "enum",
            ParamType::Path => "path",
            ParamType::Object { .. } => "object",
        }
    }

    /// Validate a raw string edit value against this type.
    ///
    /// Returns `Ok(())` if the value is acceptable, or an error message that
    /// can be shown to the user in the footer.
    pub fn validate(&self, raw: &str) -> Result<(), String> {
        match self {
            ParamType::Bool => {
                if raw == "true" || raw == "false" {
                    Ok(())
                } else {
                    Err(format!("must be 'true' or 'false', got '{raw}'"))
                }
            }
            ParamType::U64 { min, max } => raw
                .parse::<u64>()
                .map_err(|_| format!("must be an integer, got '{raw}'"))
                .and_then(|v| {
                    if v >= *min && v <= *max {
                        Ok(())
                    } else {
                        Err(format!("must be between {min} and {max}, got {v}"))
                    }
                }),
            ParamType::F64 { min, max } => raw
                .parse::<f64>()
                .map_err(|_| format!("must be a number, got '{raw}'"))
                .and_then(|v| {
                    if v >= *min && v <= *max {
                        Ok(())
                    } else {
                        Err(format!("must be between {min} and {max}, got {v}"))
                    }
                }),
            ParamType::String | ParamType::Path => Ok(()),
            ParamType::Enum { values } => {
                if values.contains(&raw) {
                    Ok(())
                } else {
                    Err(format!(
                        "must be one of [{}], got '{raw}'",
                        values.join(", ")
                    ))
                }
            }
            ParamType::Object { .. } => Ok(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter definition
// ---------------------------------------------------------------------------

/// Whether a parameter change can be applied to a running node via SIGHUP
/// or requires a full process restart to take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reloadability {
    /// Parameter can be applied live via SIGHUP without a process restart.
    Hot,
    /// Parameter requires a process restart to take effect.
    Restart,
}

impl Reloadability {
    /// Short indicator string shown next to the parameter key in the TUI.
    pub fn indicator(self) -> &'static str {
        match self {
            Reloadability::Hot => "[H]",
            Reloadability::Restart => "[R]",
        }
    }
}

// ---------------------------------------------------------------------------
// Sub-parameter definition (for fields inside an Object param)
// ---------------------------------------------------------------------------

/// A single field inside a [`ParamType::Object`].
///
/// Identical in shape to [`ParamDef`] except for the missing `section` field —
/// sub-fields live under their parent's section.
///
/// A sub-field's `default` follows the same rules as `ParamDef::default`. An
/// empty string for a numeric / Bool / Enum leaf signals "no schema default"
/// (the leaf is not hydrated and only appears in the tree if present in the
/// on-disk file). For String / Path leaves, an empty default is a valid empty
/// string.
#[derive(Debug, Clone, PartialEq)]
pub struct SubParamDef {
    pub key: &'static str,
    pub param_type: ParamType,
    pub default: &'static str,
    pub description: &'static str,
    pub tuning_hint: &'static str,
    pub reloadability: Reloadability,
}

impl SubParamDef {
    /// Parse the sub-field's default into a JSON value matching its type, or
    /// `None` if the default string cannot be parsed (numeric / Bool / Enum
    /// with `default: ""`).
    pub fn default_as_json(&self) -> Option<Value> {
        parse_default_as_json(&self.param_type, self.default)
    }
}

/// Parse a default-string into a JSON value matching `param_type`. Returns
/// `None` for unparseable numeric/Bool/Enum defaults (e.g. empty string on a
/// numeric type, which the schema uses to mean "no schema default"). For
/// Object types, recursively synthesises a `Value::Object` from `fields`.
pub(crate) fn parse_default_as_json(param_type: &ParamType, default: &str) -> Option<Value> {
    match param_type {
        ParamType::Bool => default.parse::<bool>().ok().map(Value::Bool),
        ParamType::U64 { .. } => default
            .parse::<u64>()
            .ok()
            .map(|n| Value::Number(serde_json::Number::from(n))),
        ParamType::F64 { .. } => default
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number),
        ParamType::String | ParamType::Path | ParamType::Enum { .. } => {
            Some(Value::String(default.to_string()))
        }
        ParamType::Object { fields } => Some(object_default(fields)),
    }
}

/// Recursively synthesise a `Value::Object` from a list of sub-field defaults.
/// Leaves with no parseable default (e.g. numeric with `default: ""`) are
/// omitted from the result.
pub(crate) fn object_default(fields: &[SubParamDef]) -> Value {
    let mut map = serde_json::Map::new();
    for sub in fields {
        if let Some(v) = sub.default_as_json() {
            map.insert(sub.key.to_string(), v);
        }
    }
    Value::Object(map)
}

// ---------------------------------------------------------------------------
// Parameter definition
// ---------------------------------------------------------------------------

/// A single known configuration parameter.
#[derive(Debug, Clone)]
pub struct ParamDef {
    /// The JSON key exactly as it appears in the config file.
    pub key: &'static str,
    /// The logical section this parameter belongs to.
    pub section: &'static str,
    /// The value type (drives validation and editor mode).
    pub param_type: ParamType,
    /// Default value as a display string (informational only).
    pub default: &'static str,
    /// Human-readable description shown in the right-hand description panel.
    pub description: &'static str,
    /// Practical operator tuning guidance shown below the description.
    ///
    /// An empty string means no hint is shown.  Hints explain the *why*
    /// behind a setting — what to change and when — rather than repeating
    /// what the description already says.
    pub tuning_hint: &'static str,
    /// Whether a runtime change to this parameter can be applied
    /// via SIGHUP or requires a full process restart.
    pub reloadability: Reloadability,
}

impl ParamDef {
    /// Parse this parameter's documented [`default`](Self::default) into a
    /// JSON value matching its [`param_type`](Self::param_type). Returns
    /// `None` only when the default string cannot be parsed for the given
    /// type (e.g. an empty string for a numeric type) — the TUI then skips
    /// surfacing this parameter as an unset row.
    ///
    /// Used by [`crate::config::LoadedConfig::inject_schema_defaults`] to
    /// show every schema parameter in the TUI with its default value, and by
    /// [`crate::config::save_config`] to decide whether a synthetic entry has
    /// drifted from its default (and therefore needs to be persisted).
    pub fn default_as_json(&self) -> Option<Value> {
        parse_default_as_json(&self.param_type, self.default)
    }
}

// ---------------------------------------------------------------------------
// Known parameter table
// ---------------------------------------------------------------------------

/// All known Cardano node configuration parameters, in section/display order.
///
/// When a key from the loaded JSON file matches an entry here, its metadata is
/// used for display and validation. Unknown keys fall back to raw-string
/// editing under the [`SECTION_UNKNOWN`] section.
pub static KNOWN_PARAMS: &[ParamDef] = &[
    // --- Network section ---------------------------------------------------
    ParamDef {
        key: "Network",
        section: "Network",
        param_type: ParamType::Enum {
            values: &["Mainnet", "Testnet"],
        },
        default: "Mainnet",
        description: "Cardano network identifier. 'Mainnet' for the main chain; \
                      'Testnet' for any test network (requires NetworkMagic).",
        tuning_hint: "Set to 'Testnet' for Preview/Preprod/private deployments \
                      and ensure NetworkMagic matches the genesis file.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "NetworkMagic",
        section: "Network",
        param_type: ParamType::U64 {
            min: 0,
            max: u64::MAX,
        },
        default: "764824073",
        description: "Network magic number. Mainnet = 764824073, Preview = 2, Preprod = 1. \
                      Must match the genesis files and all connecting peers.",
        tuning_hint: "Mainnet = 764824073, Preview = 2, Preprod = 1. \
                      A mismatched magic will cause all peer handshakes to fail immediately.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "RequiresNetworkMagic",
        section: "Network",
        param_type: ParamType::Enum {
            values: &["RequiresNoMagic", "RequiresMagic"],
        },
        default: "RequiresMagic",
        description: "Controls whether the network magic is enforced on peer handshakes. \
                      Use 'RequiresMagic' for all non-mainnet deployments.",
        tuning_hint: "Use 'RequiresMagic' for all testnets, 'RequiresNoMagic' for mainnet.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "DiffusionMode",
        section: "Network",
        param_type: ParamType::Enum {
            values: &["InitiatorOnly", "InitiatorAndResponder"],
        },
        default: "InitiatorAndResponder",
        description: "Controls inbound connection acceptance. 'InitiatorAndResponder' \
                      (default) opens a listening port and accepts inbound N2N connections — \
                      the correct mode for relay nodes. 'InitiatorOnly' makes only outbound \
                      connections, suitable for block producers behind a firewall.",
        tuning_hint: "Use 'InitiatorAndResponder' for relays and public-facing nodes. \
                      Use 'InitiatorOnly' for block producers that should never accept \
                      unsolicited inbound connections.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "PeerSharing",
        section: "Network",
        param_type: ParamType::Bool,
        default: "false",
        description: "Enable peer sharing mini-protocol. When true, this node \
                      advertises known peers to requesting peers. Automatically \
                      disabled for block producers (when KES/VRF keys are provided) \
                      and enabled for relays when not set explicitly. Setting this \
                      field overrides the automatic detection.",
        tuning_hint: "Leave unset to use the automatic default (enabled for relays, \
                      disabled for block producers). Only set explicitly if you need \
                      to override the detection.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TargetNumberOfActivePeers",
        section: "Network",
        param_type: ParamType::U64 { min: 1, max: 100 },
        default: "20",
        description: "Target number of fully active (hot) peers — connections where \
                      block headers and bodies are exchanged. Raising this improves \
                      propagation at the cost of higher CPU and bandwidth.",
        tuning_hint: "20 is the cardano-node default and suits most relays. \
                      Block producers may want 10-15 for lower latency and less noise.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "TargetNumberOfEstablishedPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 1, max: 1000 },
        default: "30",
        description: "Target number of established (warm) peers — TCP connections that \
                      are open but not yet doing full block exchange. Acts as a reservoir \
                      to promote to hot when needed.",
        tuning_hint: "Keep at 1.5-2x TargetNumberOfActivePeers to ensure a healthy \
                      promotion reservoir. 30 is the cardano-node default.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "TargetNumberOfKnownPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 1, max: 10000 },
        default: "150",
        description: "Target size of the known-peers set (cold + warm + hot). The peer \
                      governor will attempt to keep at least this many addresses in its \
                      address book at all times.",
        tuning_hint: "150 is the cardano-node default. \
                      Increase to 200+ for higher network resilience on busy relays.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "TargetNumberOfRootPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 1, max: 1000 },
        default: "60",
        description: "Target number of root peers — connections maintained to the \
                      topology file entries (trusted relays). These anchor the node to \
                      the network before ledger peer discovery kicks in.",
        tuning_hint: "Match or slightly exceed your topology file entry count. \
                      Root peers keep the node anchored during initial bootstrap.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "TargetNumberOfActiveBigLedgerPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 50 },
        default: "5",
        description: "Target number of active connections to 'big ledger' peers — \
                      well-staked SPO relays discovered from the on-chain pool params \
                      after useLedgerAfterSlot is reached.",
        tuning_hint: "5-10 is sufficient for most relays. \
                      Big ledger peers are high-quality but may be geographically distant.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "TargetNumberOfEstablishedBigLedgerPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 100 },
        default: "10",
        description: "Target number of established (warm) connections to big ledger peers.",
        tuning_hint: "Keep at 2x TargetNumberOfActiveBigLedgerPeers \
                      to allow smooth promotion without cold-start delays.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "TargetNumberOfKnownBigLedgerPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 200 },
        default: "15",
        description: "Target size of the known big-ledger-peer set (cold + warm + hot).",
        tuning_hint: "15-25 gives a good pool of candidates for ledger peer selection \
                      without excessive churn.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "ConsensusMode",
        section: "Network",
        param_type: ParamType::Enum {
            values: &["PraosMode", "GenesisMode"],
        },
        default: "PraosMode",
        description: "Consensus protocol mode. PraosMode is the standard operating mode. \
                      GenesisMode enables Ouroboros Genesis for trustless bulk sync from \
                      potentially dishonest peers.",
        tuning_hint: "Use PraosMode unless you specifically need Genesis sync guarantees. \
                      GenesisMode requires additional SyncTargetNumberOf* configuration.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "SyncTargetNumberOfActivePeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 100 },
        default: "5",
        description: "Target active peers during Genesis bulk sync. Only used when \
                      ConsensusMode is GenesisMode.",
        tuning_hint: "5 is the cardano-node default. For GenesisMode, match or \
                      raise your regular TargetNumberOfActivePeers for aggressive sync.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "SyncTargetNumberOfEstablishedPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 1000 },
        default: "10",
        description: "Target established peers during Genesis bulk sync.",
        tuning_hint: "10 is the cardano-node default.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "SyncTargetNumberOfKnownPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 10000 },
        default: "150",
        description: "Target known peers during Genesis bulk sync.",
        tuning_hint: "150 is the cardano-node default.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "SyncTargetNumberOfRootPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 1000 },
        default: "0",
        description: "Target root peers during Genesis bulk sync.",
        tuning_hint: "0 is the cardano-node default (root peers not needed during Genesis sync).",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "SyncTargetNumberOfActiveBigLedgerPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 100 },
        default: "30",
        description: "Target active big ledger peers during Genesis bulk sync. \
                      High value ensures honest chain availability during sync.",
        tuning_hint: "30 is the cardano-node default. Higher values improve Genesis safety \
                      at the cost of more connections during sync.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "SyncTargetNumberOfEstablishedBigLedgerPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 1000 },
        default: "40",
        description: "Target established big ledger peers during Genesis bulk sync.",
        tuning_hint: "40 is the cardano-node default.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "SyncTargetNumberOfKnownBigLedgerPeers",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 10000 },
        default: "100",
        description: "Target known big ledger peers during Genesis bulk sync.",
        tuning_hint: "100 is the cardano-node default.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "MinBigLedgerPeersForTrustedState",
        section: "Network",
        param_type: ParamType::U64 { min: 0, max: 100 },
        default: "5",
        description: "Minimum active big ledger peers required to continue syncing in \
                      Genesis mode. If active BLPs drop below this, block adoption is \
                      paused until enough connections recover.",
        tuning_hint: "5 is the Haskell default. Lower values reduce safety guarantees. \
                      Only relevant when ConsensusMode is GenesisMode.",
        reloadability: Reloadability::Restart,
    },
    // --- Genesis section ---------------------------------------------------
    ParamDef {
        key: "ByronGenesisFile",
        section: "Genesis",
        param_type: ParamType::Path,
        default: "byron-genesis.json",
        description: "Path to the Byron-era genesis JSON file. Can be relative to the \
                      config file's directory or absolute. Must match ByronGenesisHash.",
        tuning_hint: "Must match the network. Do not change unless switching networks. \
                      Use paths relative to the config file for portability.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "ByronGenesisHash",
        section: "Genesis",
        param_type: ParamType::String,
        default: "",
        description: "Blake2b-256 hash (hex) of the Byron genesis file. \
                      The node verifies this on startup to detect genesis mismatches.",
        tuning_hint: "Must exactly match the hash of the genesis file at ByronGenesisFile. \
                      An incorrect hash will prevent the node from starting.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "ShelleyGenesisFile",
        section: "Genesis",
        param_type: ParamType::Path,
        default: "shelley-genesis.json",
        description: "Path to the Shelley-era genesis JSON file. Contains network \
                      parameters, initial delegation, protocol magic, and epoch length.",
        tuning_hint: "Must match the network. Do not change unless switching networks.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "ShelleyGenesisHash",
        section: "Genesis",
        param_type: ParamType::String,
        default: "",
        description: "Blake2b-256 hash (hex) of the Shelley genesis file.",
        tuning_hint: "Must exactly match the hash of the file at ShelleyGenesisFile.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "AlonzoGenesisFile",
        section: "Genesis",
        param_type: ParamType::Path,
        default: "alonzo-genesis.json",
        description: "Path to the Alonzo-era genesis JSON file. Contains initial Plutus \
                      cost model parameters and collateral percentage.",
        tuning_hint: "Must match the network. Do not change unless switching networks.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "AlonzoGenesisHash",
        section: "Genesis",
        param_type: ParamType::String,
        default: "",
        description: "Blake2b-256 hash (hex) of the Alonzo genesis file.",
        tuning_hint: "Must exactly match the hash of the file at AlonzoGenesisFile.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "ConwayGenesisFile",
        section: "Genesis",
        param_type: ParamType::Path,
        default: "conway-genesis.json",
        description: "Path to the Conway-era genesis JSON file. Contains governance \
                      bootstrap DReps, committee members, and Plutus V3 cost models.",
        tuning_hint: "Must match the network. Do not change unless switching networks.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "ConwayGenesisHash",
        section: "Genesis",
        param_type: ParamType::String,
        default: "",
        description: "Blake2b-256 hash (hex) of the Conway genesis file.",
        tuning_hint: "Must exactly match the hash of the file at ConwayGenesisFile.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "DijkstraGenesisFile",
        section: "Genesis",
        param_type: ParamType::Path,
        default: "dijkstra-genesis.json",
        description: "Path to the Dijkstra-era genesis JSON file. Carries the four \
                      reference-script protocol parameters introduced at the \
                      Conway-to-Dijkstra HFC (maxRefScriptSizePerBlock, \
                      maxRefScriptSizePerTx, refScriptCostStride, \
                      refScriptCostMultiplier).",
        tuning_hint: "Must match the network. Do not change unless switching networks.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "DijkstraGenesisHash",
        section: "Genesis",
        param_type: ParamType::String,
        default: "",
        description: "Blake2b-256 hash (hex) of the Dijkstra genesis file.",
        tuning_hint: "Must exactly match the hash of the file at DijkstraGenesisFile.",
        reloadability: Reloadability::Restart,
    },
    // --- Protocol section --------------------------------------------------
    ParamDef {
        key: "Protocol",
        section: "Protocol",
        param_type: ParamType::Enum {
            values: &["Cardano", "TPraos", "Praos"],
        },
        default: "Cardano",
        description: "Consensus protocol. 'Cardano' runs the full Hard Fork Combinator \
                      covering all eras from Byron to Conway. 'TPraos' and 'Praos' are \
                      single-era modes used only for isolated test networks.",
        tuning_hint: "Always use 'Cardano' for mainnet and public testnets. \
                      'TPraos'/'Praos' are for private devnet experiments only.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceBlockFetchClient",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit detailed block-fetch client trace events. Useful for \
                      diagnosing slow block propagation but very verbose at high sync rates.",
        tuning_hint: "Enable only for debugging slow block propagation. \
                      Increases log volume significantly; disable in production.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceBlockFetchServer",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit detailed block-fetch server trace events (blocks served \
                      to downstream peers).",
        tuning_hint: "Enable only for debugging. Increases log volume significantly.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceChainSyncClient",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit chain-sync client trace events (header fetch from upstream).",
        tuning_hint: "Enable only for debugging chain-sync issues. \
                      Very verbose during initial sync; keep off in production.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceChainSyncHeaderServer",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit chain-sync header server trace events (headers served to \
                      downstream peers).",
        tuning_hint: "Enable only for debugging. Increases log volume significantly.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceChainSyncBlockServer",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit chain-sync block server trace events.",
        tuning_hint: "Enable only for debugging. Increases log volume significantly.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceChainDb",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit ChainDB trace events (block storage, volatile/immutable flush, \
                      rollback operations). Useful for diagnosing storage-layer issues.",
        tuning_hint: "Enable to debug block storage problems or unexpected rollbacks. \
                      Moderate log volume; safe to leave enabled in production if needed.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceChainSyncServer",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit chain-sync server trace events (both header and block serving \
                      to downstream N2N peers).",
        tuning_hint: "Enable only for debugging downstream sync issues. \
                      Increases log volume significantly under heavy peer load.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceForge",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit block forging trace events (VRF leader check, block construction, \
                      KES signing, block announcement). Essential for block producer debugging.",
        tuning_hint: "Enable on block producers to diagnose missed slots or forging failures. \
                      Low volume (one event per slot check); safe for production.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TraceMempool",
        section: "Protocol",
        param_type: ParamType::Bool,
        default: "false",
        description: "Emit mempool trace events (transaction admission, rejection, removal \
                      on block application, TTL expiry).",
        tuning_hint: "Enable to debug transaction flow or mempool capacity issues. \
                      Volume depends on transaction rate; moderate on mainnet.",
        reloadability: Reloadability::Restart,
    },
    // --- Logging section ---------------------------------------------------
    ParamDef {
        key: "MinSeverity",
        section: "Logging",
        param_type: ParamType::Enum {
            values: &[
                "Debug",
                "Info",
                "Notice",
                "Warning",
                "Error",
                "Critical",
                "Alert",
                "Emergency",
            ],
        },
        default: "Info",
        description: "Minimum log severity. Messages below this level are silently \
                      discarded. 'Debug' is very verbose; 'Warning' is suitable for \
                      production deployments.",
        tuning_hint: "'Info' is the recommended default. \
                      Use 'Warning' for quiet production nodes. \
                      Use 'Debug' only for active troubleshooting sessions.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "LogDirective",
        section: "Logging",
        param_type: ParamType::String,
        default: "",
        description: "Per-subsystem trace filter directive in tracing_subscriber EnvFilter \
                      syntax. Levels (low to high verbosity): error, warn, info, debug, trace \
                      (lowercase — distinct from MinSeverity's syslog-style 'Info'/'Warning'). \
                      A directive is a comma-separated list of '<target>=<level>' pairs, where \
                      a bare level acts as the global default for all targets. \
                      Examples: \
                      'info' (global INFO); \
                      'debug,hyper=warn' (DEBUG globally, quiet hyper); \
                      'info,dugite_network=trace,dugite_consensus=debug' (per-subsystem); \
                      'warn,dugite_network::chainsync=trace' (per-module within a crate); \
                      'off,dugite_ledger=info' (silence everything except the ledger). \
                      Applied on SIGHUP without a process restart (commit 1f34ac81c). \
                      If absent, the --log-level CLI flag value remains in effect. \
                      Equivalent to the RUST_LOG environment variable for startup.",
        tuning_hint: "Edit this field and send SIGHUP to reload log verbosity at runtime \
                      without restarting the node. Useful for diagnosing live issues. \
                      Start broad ('debug') then narrow to the noisy subsystem \
                      ('info,dugite_network=trace') once you've located it. \
                      Leave empty to use the startup --log-level value.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "TurnOnLogMetrics",
        section: "Logging",
        param_type: ParamType::Bool,
        default: "true",
        description: "Enable the EKG / Prometheus metrics endpoint. When true, metrics \
                      are published on port 12798 and can be scraped by Prometheus.",
        tuning_hint: "Keep enabled. Disabling removes Prometheus scraping capability \
                      and breaks monitoring dashboards.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TurnOnScripting",
        section: "Logging",
        param_type: ParamType::Bool,
        default: "false",
        description: "Enable scripted log routing (cardano-node legacy logging system). \
                      Not applicable to Dugite's tracing-subscriber backend.",
        tuning_hint: "Leave disabled for Dugite. This setting is a legacy flag \
                      that has no effect on Dugite's tracing-subscriber backend.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "MetricsPort",
        section: "Logging",
        param_type: ParamType::U64 { min: 0, max: 65535 },
        default: "12798",
        description: "TCP port for the Prometheus metrics endpoint. Set to 0 to disable \
                      the metrics server entirely. The CLI flag --metrics-port takes \
                      precedence over this config value; --no-metrics forces port to 0.",
        tuning_hint: "12798 (default) matches cardano-node. Change only if the port \
                      conflicts with another service. Set to 0 in hardened environments \
                      where metrics scraping is not needed.",
        reloadability: Reloadability::Restart,
    },
    // --- Advanced section --------------------------------------------------
    ParamDef {
        key: "MaxConcurrencyBulkSync",
        section: "Advanced",
        param_type: ParamType::U64 { min: 1, max: 64 },
        default: "2",
        description: "Maximum number of parallel block-fetch workers during bulk \
                      (catch-up) sync. Higher values saturate bandwidth faster at the \
                      cost of higher memory usage.",
        tuning_hint: "4-8 on fast hardware/NVMe with ample RAM. \
                      Lower to 2 if memory is constrained below 8 GB.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "MaxConcurrencyDeadline",
        section: "Advanced",
        param_type: ParamType::U64 { min: 1, max: 32 },
        default: "4",
        description: "Maximum number of parallel block-fetch workers when near the tip \
                      (deadline mode). Lower than bulk to reduce latency jitter.",
        tuning_hint: "Keep lower than MaxConcurrencyBulkSync. \
                      2-4 is optimal; higher values add latency jitter near tip.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "SnapshotInterval",
        section: "Advanced",
        param_type: ParamType::U64 {
            min: 0,
            max: 86_400,
        },
        default: "72",
        description: "Interval in minutes between ledger state snapshots. \
                      Snapshots allow faster restart after an unclean shutdown. \
                      0 disables periodic snapshotting (not recommended).",
        tuning_hint: "72 minutes (default) matches the Haskell node. \
                      Never set to 0 in production — recovery from an unclean \
                      shutdown will require a full replay from genesis.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "ExperimentalHardForksEnabled",
        section: "Advanced",
        param_type: ParamType::Bool,
        default: "false",
        description: "Allow the node to follow experimental hard fork transitions. \
                      Enable only when instructed by the Cardano Foundation for \
                      testnet protocol upgrades.",
        tuning_hint: "Leave disabled unless you have been explicitly asked to enable it \
                      for a specific testnet upgrade. Enabling prematurely can cause \
                      chain divergence on mainnet.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "ChurnIntervalNormalSecs",
        section: "Advanced",
        param_type: ParamType::U64 {
            min: 60,
            max: 86_400,
        },
        default: "3300",
        description: "Peer governor churn interval during normal (caught-up) operation, \
                      in seconds. Controls how often the governor rotates a random subset \
                      of peers to prevent the node from becoming permanently attached to \
                      the same peer set. Default 3300 s (55 minutes) matches cardano-node.",
        tuning_hint: "Lower values increase peer diversity at the cost of more handshakes. \
                      Block producers may prefer higher values (3600+) for connection stability.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "ChurnIntervalSyncSecs",
        section: "Advanced",
        param_type: ParamType::U64 {
            min: 30,
            max: 86_400,
        },
        default: "900",
        description: "Peer governor churn interval during syncing, in seconds. Faster \
                      rotation while catching up allows the node to quickly shed \
                      unresponsive peers. Default 900 s (15 minutes) matches cardano-node.",
        tuning_hint: "Keep below 15 minutes to shed unresponsive peers during catch-up. \
                      Lower values improve sync speed at the cost of more connection churn.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "StallDemotionCycles",
        section: "Advanced",
        param_type: ParamType::U64 { min: 1, max: 100 },
        default: "6",
        description: "Number of consecutive governor evaluation cycles (each ~30 s) in \
                      which a hot peer must serve zero new blocks before it is demoted \
                      back to warm. Default of 6 cycles = 3 minutes of inactivity.",
        tuning_hint: "Increase if hot peers legitimately produce zero blocks for extended \
                      periods (e.g., low-stake pools). Decrease for aggressive stall detection.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "ErrorDemotionThreshold",
        section: "Advanced",
        param_type: ParamType::U64 { min: 1, max: 100 },
        default: "5",
        description: "Failure count threshold above which a hot peer is unconditionally \
                      demoted to warm during each governor evaluation cycle. Local root \
                      peers are exempt from this check.",
        tuning_hint: "Lower to aggressively shed failing peers. Raise if peers are being \
                      demoted too frequently due to transient network issues.",
        reloadability: Reloadability::Hot,
    },
    ParamDef {
        key: "ProtocolIdleTimeout",
        section: "Advanced",
        param_type: ParamType::U64 { min: 1, max: 3600 },
        default: "5",
        description: "Time in seconds before an idle mini-protocol connection is pruned.",
        tuning_hint: "5 seconds (default) matches Haskell. Increase if peers have high \
                      latency and idle connections are being pruned prematurely.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "TimeWaitTimeout",
        section: "Advanced",
        param_type: ParamType::U64 { min: 1, max: 3600 },
        default: "60",
        description: "Duration in seconds a connection stays in TIME_WAIT after close.",
        tuning_hint: "60 seconds (default) matches Haskell. Rarely needs changing.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "EgressPollInterval",
        section: "Advanced",
        param_type: ParamType::F64 {
            min: 0.0,
            max: 3600.0,
        },
        default: "0",
        description: "How often (in seconds) the outbound governor polls for new \
                      connection opportunities. 0 means the governor runs on-demand \
                      as events arrive (the Haskell default). Accepts fractional seconds.",
        tuning_hint: "0 (default) matches Haskell: governor runs event-driven. \
                      Non-zero values introduce a polling interval and reduce CPU usage \
                      at the cost of slower peer promotion response.",
        reloadability: Reloadability::Restart,
    },
    ParamDef {
        key: "ChainSyncIdleTimeout",
        section: "Advanced",
        param_type: ParamType::F64 {
            min: 0.0,
            max: 3600.0,
        },
        default: "0",
        description: "ChainSync-specific idle timeout in seconds. 0 disables the timeout. \
                      If set, a ChainSync session that produces no messages for this many \
                      seconds is closed and the peer is demoted. Accepts fractional seconds.",
        tuning_hint: "Leave at 0 (no timeout) for most deployments. \
                      Set to 300-600 to aggressively shed stalled peers.",
        reloadability: Reloadability::Restart,
    },
    // --- Diffusion section ------------------------------------------------
    ParamDef {
        key: "AcceptedConnectionsLimit",
        section: "Diffusion",
        param_type: ParamType::Object { fields: &[] },
        default: "",
        description: "Inbound connection admission limits. 'hardLimit' is the maximum \
                      concurrent inbound connections (new connections are refused above \
                      this). 'softLimit' is the threshold above which new connections \
                      are progressively delayed by up to 'delay' seconds. \
                      Matches Haskell's AcceptedConnectionsLimit with short keys \
                      hardLimit/softLimit/delay (old long camelCase aliases also accepted).",
        tuning_hint: "hardLimit=512, softLimit=384, delay=5.0 are the cardano-node defaults. \
                      Lower hardLimit on memory-constrained relays. \
                      Raise delay (up to 30s) to slow down aggressive inbound peers.",
        reloadability: Reloadability::Restart,
    },
    // --- Rpc section ------------------------------------------------------
    //
    // UTxO RPC (gRPC) server — issue #672. The full block is exposed as
    // a read-only Object so operators can see what's configured at a
    // glance; edits to sub-fields go through the config JSON directly.
    // CLI flags --rpc-host / --rpc-port / --no-rpc override at startup.
    ParamDef {
        key: "Rpc",
        section: "Rpc",
        param_type: ParamType::Object { fields: &[] },
        // Empty object → server disabled. Operators opt in by setting
        // "Enabled": true and tuning the remaining fields.
        default: "",
        description: "UTxO RPC (gRPC) server configuration. Sub-fields (all PascalCase): \
                      'Enabled': bool (default false — server off unless explicitly enabled); \
                      'ListenAddr': bind IP, default '127.0.0.1' (loopback only); \
                      'Port': TCP port, default 50051; \
                      'MaxConcurrentStreams': HTTP/2 streams/conn cap (default 64); \
                      'StreamBufferSize': per-stream event buffer (default 256); \
                      'ReflectionEnabled': bool, gRPC reflection (default true); \
                      'WebEnabled': bool, accept gRPC-Web/HTTP1.1 (default false); \
                      'AlphaEnabled': bool, expose v1alpha alongside v1beta (default true); \
                      'Tls': { 'CertPath', 'KeyPath' } for optional TLS termination. \
                      CLI flags --rpc-host / --rpc-port force-enable; --no-rpc force-disables.",
        tuning_hint: "Default-disabled to keep the gRPC stack out of the runtime when \
                      not needed. Enable for integrator/indexer workloads. Keep ListenAddr \
                      as 127.0.0.1 unless you've terminated TLS at an upstream proxy or \
                      enabled 'Tls' here directly — the loopback default protects against \
                      exposing an unauthenticated TCP gRPC endpoint to the network. \
                      Enable WebEnabled only when serving browser dApps directly.",
        reloadability: Reloadability::Restart,
    },
    // --- Storage section --------------------------------------------------
    ParamDef {
        key: "Storage",
        section: "Storage",
        param_type: ParamType::Object { fields: &[] },
        default: "",
        description: "Storage subsystem configuration. Sub-fields (all optional): \
                      'profile': preset profile name ('ultra-memory', 'high-memory', \
                      'low-memory', 'minimal'); \
                      'immutableIndexType': 'mmap' (default) or 'in-memory'; \
                      'mmapLoadFactor': mmap hash table load factor (0.0-1.0, default 0.7); \
                      'utxoBackend': 'lsm' (default) or 'in-memory'; \
                      'utxoMemtableSizeMb': LSM memtable size in MB; \
                      'utxoBlockCacheSizeMb': LSM block cache size in MB; \
                      'utxoBloomFilterBits': LSM bloom filter bits per key (default 10).",
        tuning_hint: "Start with a profile matching your RAM: 'high-memory' for 16GB, \
                      'low-memory' for 8GB, 'minimal' for 4GB. \
                      Set 'utxoBackend' to 'lsm' for production (recommended). \
                      CLI flags --storage-profile / --utxo-* take precedence over this field.",
        reloadability: Reloadability::Restart,
    },
];

// ---------------------------------------------------------------------------
// Lookup index
// ---------------------------------------------------------------------------

/// Build a lookup map from JSON key name to the corresponding [`ParamDef`].
///
/// Only the first occurrence of a key is used (the static table is deduplicated
/// in definition order). The returned map is suitable for O(1) lookups during
/// config file parsing.
pub fn build_lookup() -> HashMap<&'static str, &'static ParamDef> {
    let mut map = HashMap::new();
    for def in KNOWN_PARAMS {
        // First occurrence wins — avoids duplicates.
        map.entry(def.key).or_insert(def);
    }
    map
}

// ---------------------------------------------------------------------------
// Section ordering
// ---------------------------------------------------------------------------

/// Canonical display order for sections in the left-panel tree.
///
/// Sections not listed here are appended after the last known section,
/// with [`SECTION_UNKNOWN`] always last.
pub const SECTION_ORDER: &[&str] = &[
    "Network",
    "Genesis",
    "Protocol",
    "Logging",
    "Advanced",
    "Diffusion",
    "Storage",
    "Rpc",
];

/// Return the display priority index of a section name (lower = earlier).
pub fn section_priority(section: &str) -> usize {
    SECTION_ORDER
        .iter()
        .position(|s| *s == section)
        .unwrap_or(SECTION_ORDER.len())
}

// ---------------------------------------------------------------------------
// Network default values (used by `init` subcommand)
// ---------------------------------------------------------------------------

/// The recognised networks for the `init` subcommand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Network {
    Mainnet,
    Preview,
    Preprod,
}

impl Network {
    /// Parse a network name string (case-insensitive).
    pub fn from_str(s: &str) -> Option<Network> {
        match s.to_lowercase().as_str() {
            "mainnet" => Some(Network::Mainnet),
            "preview" => Some(Network::Preview),
            "preprod" => Some(Network::Preprod),
            _ => None,
        }
    }

    /// The network magic integer for this network.
    pub fn magic(self) -> u64 {
        match self {
            Network::Mainnet => 764_824_073,
            Network::Preview => 2,
            Network::Preprod => 1,
        }
    }

    /// Whether network magic enforcement is needed (mainnet uses RequiresNoMagic).
    pub fn requires_magic(self) -> &'static str {
        match self {
            Network::Mainnet => "RequiresNoMagic",
            Network::Preview | Network::Preprod => "RequiresMagic",
        }
    }

    /// Display name used in genesis file path prefixes.
    pub fn genesis_prefix(self) -> &'static str {
        match self {
            Network::Mainnet => "mainnet",
            Network::Preview => "preview",
            Network::Preprod => "preprod",
        }
    }
}

/// Build a `serde_json::Map` with sensible defaults for the given network.
///
/// The returned map is ready to be pretty-printed and written to disk as a
/// starter configuration file.  All paths use the conventional relative
/// names expected alongside an official Cardano node config directory.
pub fn network_defaults(network: Network) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::{json, Map, Value};

    let prefix = network.genesis_prefix();
    let magic = network.magic();
    let req_magic = network.requires_magic();
    let network_str = match network {
        Network::Mainnet => "Mainnet",
        _ => "Testnet",
    };

    let mut map = Map::new();

    // Network identity.
    map.insert("Network".into(), json!(network_str));
    map.insert("NetworkMagic".into(), json!(magic));
    map.insert("RequiresNetworkMagic".into(), json!(req_magic));

    // P2P networking.
    map.insert("DiffusionMode".into(), json!("InitiatorAndResponder"));
    // PeerSharing is a bool in NodeConfig; leave unset to use the auto-default
    // (enabled for relays, disabled for block producers).
    map.insert("TargetNumberOfActivePeers".into(), json!(20));
    map.insert("TargetNumberOfEstablishedPeers".into(), json!(30));
    map.insert("TargetNumberOfKnownPeers".into(), json!(150));
    map.insert("TargetNumberOfRootPeers".into(), json!(60));
    map.insert("TargetNumberOfActiveBigLedgerPeers".into(), json!(5));
    map.insert("TargetNumberOfEstablishedBigLedgerPeers".into(), json!(10));
    map.insert("TargetNumberOfKnownBigLedgerPeers".into(), json!(15));

    // Consensus mode.
    map.insert("ConsensusMode".into(), json!("PraosMode"));

    // Genesis sync targets (cardano-node defaults).
    map.insert("SyncTargetNumberOfActivePeers".into(), json!(5));
    map.insert("SyncTargetNumberOfEstablishedPeers".into(), json!(10));
    map.insert("SyncTargetNumberOfKnownPeers".into(), json!(150));
    map.insert("SyncTargetNumberOfRootPeers".into(), json!(0));
    map.insert("SyncTargetNumberOfActiveBigLedgerPeers".into(), json!(30));
    map.insert(
        "SyncTargetNumberOfEstablishedBigLedgerPeers".into(),
        json!(40),
    );
    map.insert("SyncTargetNumberOfKnownBigLedgerPeers".into(), json!(100));
    map.insert("MinBigLedgerPeersForTrustedState".into(), json!(5));

    // Genesis files (conventional relative paths).
    map.insert(
        "ByronGenesisFile".into(),
        Value::String(format!("{prefix}-byron-genesis.json")),
    );
    map.insert("ByronGenesisHash".into(), json!(""));
    map.insert(
        "ShelleyGenesisFile".into(),
        Value::String(format!("{prefix}-shelley-genesis.json")),
    );
    map.insert("ShelleyGenesisHash".into(), json!(""));
    map.insert(
        "AlonzoGenesisFile".into(),
        Value::String(format!("{prefix}-alonzo-genesis.json")),
    );
    map.insert("AlonzoGenesisHash".into(), json!(""));
    map.insert(
        "ConwayGenesisFile".into(),
        Value::String(format!("{prefix}-conway-genesis.json")),
    );
    map.insert("ConwayGenesisHash".into(), json!(""));
    map.insert(
        "DijkstraGenesisFile".into(),
        Value::String(format!("{prefix}-dijkstra-genesis.json")),
    );
    map.insert("DijkstraGenesisHash".into(), json!(""));

    // Protocol.
    map.insert("Protocol".into(), json!("Cardano"));
    map.insert("TraceBlockFetchClient".into(), json!(false));
    map.insert("TraceBlockFetchServer".into(), json!(false));
    map.insert("TraceChainSyncClient".into(), json!(false));
    map.insert("TraceChainSyncHeaderServer".into(), json!(false));
    map.insert("TraceChainSyncBlockServer".into(), json!(false));
    map.insert("TraceChainDb".into(), json!(false));
    map.insert("TraceChainSyncServer".into(), json!(false));
    map.insert("TraceForge".into(), json!(false));
    map.insert("TraceMempool".into(), json!(false));

    // Logging.
    map.insert("MinSeverity".into(), json!("Info"));
    // LogDirective is optional — omit from defaults so SIGHUP is a no-op unless set.
    map.insert("TurnOnLogMetrics".into(), json!(true));
    map.insert("TurnOnScripting".into(), json!(false));
    map.insert("MetricsPort".into(), json!(12798));

    // Advanced.
    map.insert("MaxConcurrencyBulkSync".into(), json!(2));
    map.insert("MaxConcurrencyDeadline".into(), json!(4));
    map.insert("SnapshotInterval".into(), json!(72));
    map.insert("ExperimentalHardForksEnabled".into(), json!(false));
    map.insert("ChurnIntervalNormalSecs".into(), json!(3300));
    map.insert("ChurnIntervalSyncSecs".into(), json!(900));
    map.insert("StallDemotionCycles".into(), json!(6));
    map.insert("ErrorDemotionThreshold".into(), json!(5));

    // Connection management (fractional seconds, matching Haskell DiffTime).
    map.insert("ProtocolIdleTimeout".into(), json!(5));
    map.insert("TimeWaitTimeout".into(), json!(60));
    map.insert("EgressPollInterval".into(), json!(0));
    // ChainSyncIdleTimeout is optional; omit from defaults (None = no timeout).

    // Diffusion — inbound connection limits (optional; node uses hard-coded defaults when absent).
    // Uncomment and tune for relay nodes under heavy inbound pressure:
    // map.insert("AcceptedConnectionsLimit".into(), json!({
    //     "hardLimit": 512,
    //     "softLimit": 384,
    //     "delay": 5.0
    // }));

    // Storage — omit from defaults; the --storage-profile CLI flag is the preferred knob.
    // Uncomment to pin storage settings in the config file:
    // map.insert("Storage".into(), json!({"profile": "high-memory"}));

    map
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_param_type_validate_bool() {
        let t = ParamType::Bool;
        assert!(t.validate("true").is_ok());
        assert!(t.validate("false").is_ok());
        assert!(t.validate("yes").is_err());
        assert!(t.validate("1").is_err());
    }

    #[test]
    fn test_param_type_validate_u64_range() {
        let t = ParamType::U64 { min: 1, max: 100 };
        assert!(t.validate("1").is_ok());
        assert!(t.validate("100").is_ok());
        assert!(t.validate("0").is_err());
        assert!(t.validate("101").is_err());
        assert!(t.validate("abc").is_err());
    }

    #[test]
    fn test_param_type_validate_f64_range() {
        let t = ParamType::F64 {
            min: 0.0,
            max: 3600.0,
        };
        assert!(t.validate("0").is_ok());
        assert!(t.validate("0.0").is_ok());
        assert!(t.validate("3600").is_ok());
        assert!(t.validate("5.5").is_ok());
        assert!(t.validate("-1").is_err());
        assert!(t.validate("3601").is_err());
        assert!(t.validate("abc").is_err());
    }

    #[test]
    fn test_param_type_validate_enum() {
        let t = ParamType::Enum {
            values: &["A", "B", "C"],
        };
        assert!(t.validate("A").is_ok());
        assert!(t.validate("D").is_err());
    }

    #[test]
    fn test_param_type_validate_object_always_ok() {
        let t = ParamType::Object { fields: &[] };
        assert!(t.validate("").is_ok());
        assert!(t.validate("anything").is_ok());
    }

    #[test]
    fn test_build_lookup_no_key_collisions() {
        let map = build_lookup();
        // Every entry in the map should point to a real ParamDef.
        for (key, def) in &map {
            assert_eq!(*key, def.key);
        }
    }

    #[test]
    fn test_section_priority_order() {
        assert!(section_priority("Network") < section_priority("Genesis"));
        assert!(section_priority("Genesis") < section_priority("Protocol"));
        assert!(section_priority("Protocol") < section_priority("Logging"));
        assert!(section_priority("Logging") < section_priority("Advanced"));
        assert!(section_priority("Advanced") < section_priority("Diffusion"));
        assert!(section_priority("Diffusion") < section_priority("Storage"));
        assert!(section_priority("Storage") < section_priority(SECTION_UNKNOWN));
    }

    #[test]
    fn test_default_as_json_coerces_each_param_type() {
        let lookup = build_lookup();

        // Bool default parses to JSON bool.
        let v = lookup
            .get("TurnOnLogMetrics")
            .unwrap()
            .default_as_json()
            .unwrap();
        assert_eq!(v, Value::Bool(true));

        // U64 default parses to JSON number.
        let v = lookup
            .get("MaxConcurrencyBulkSync")
            .unwrap()
            .default_as_json()
            .unwrap();
        assert_eq!(v, Value::Number(2.into()));

        // String default parses to JSON string (even when empty).
        let v = lookup
            .get("LogDirective")
            .unwrap()
            .default_as_json()
            .unwrap();
        assert_eq!(v, Value::String(String::new()));

        // Enum default parses to JSON string.
        let v = lookup
            .get("MinSeverity")
            .unwrap()
            .default_as_json()
            .unwrap();
        assert_eq!(v, Value::String("Info".into()));

        // Object default synthesises an empty JSON object (fields: &[] — sub-schemas
        // are populated in Tasks 14–16).
        let v = lookup.get("Storage").unwrap().default_as_json().unwrap();
        assert!(v.as_object().is_some());
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn test_default_as_json_covers_every_param() {
        // The TUI surfaces an unset row for every schema parameter — so every
        // ParamDef must parse its default into a JSON value. Catches typos
        // (e.g. "tru" for a bool) and unsupported defaults early.
        for def in KNOWN_PARAMS {
            assert!(
                def.default_as_json().is_some(),
                "default_as_json failed for '{}' (default = {:?}, type = {:?})",
                def.key,
                def.default,
                def.param_type
            );
        }
    }

    #[test]
    fn test_param_type_label() {
        assert_eq!(ParamType::Bool.label(), "bool");
        assert_eq!(ParamType::U64 { min: 0, max: 10 }.label(), "u64");
        assert_eq!(
            ParamType::F64 {
                min: 0.0,
                max: 10.0
            }
            .label(),
            "f64"
        );
        assert_eq!(ParamType::String.label(), "string");
        assert_eq!(ParamType::Enum { values: &["a"] }.label(), "enum");
        assert_eq!(ParamType::Path.label(), "path");
        assert_eq!(ParamType::Object { fields: &[] }.label(), "object");
    }

    #[test]
    fn test_all_params_have_tuning_hints() {
        // Every entry in KNOWN_PARAMS must carry a non-empty tuning hint so
        // the description panel always has operator guidance to show.
        for def in KNOWN_PARAMS {
            assert!(
                !def.tuning_hint.is_empty(),
                "ParamDef '{}' is missing a tuning_hint",
                def.key
            );
        }
    }

    // ── Correct defaults for NodeConfig-matching params ────────────────────

    #[test]
    fn test_peer_sharing_is_bool_type() {
        let map = build_lookup();
        let def = map["PeerSharing"];
        assert_eq!(def.param_type, ParamType::Bool);
    }

    #[test]
    fn test_target_active_peers_default_is_20() {
        let map = build_lookup();
        let def = map["TargetNumberOfActivePeers"];
        assert_eq!(def.default, "20");
    }

    #[test]
    fn test_target_established_peers_default_is_30() {
        let map = build_lookup();
        let def = map["TargetNumberOfEstablishedPeers"];
        assert_eq!(def.default, "30");
    }

    #[test]
    fn test_target_known_peers_default_is_150() {
        let map = build_lookup();
        let def = map["TargetNumberOfKnownPeers"];
        assert_eq!(def.default, "150");
    }

    #[test]
    fn test_sync_target_active_peers_default_is_5() {
        let map = build_lookup();
        let def = map["SyncTargetNumberOfActivePeers"];
        assert_eq!(def.default, "5");
    }

    #[test]
    fn test_sync_target_established_peers_default_is_10() {
        let map = build_lookup();
        let def = map["SyncTargetNumberOfEstablishedPeers"];
        assert_eq!(def.default, "10");
    }

    #[test]
    fn test_sync_target_known_peers_default_is_150() {
        let map = build_lookup();
        let def = map["SyncTargetNumberOfKnownPeers"];
        assert_eq!(def.default, "150");
    }

    #[test]
    fn test_sync_established_blp_default_is_40() {
        let map = build_lookup();
        let def = map["SyncTargetNumberOfEstablishedBigLedgerPeers"];
        assert_eq!(def.default, "40");
    }

    #[test]
    fn test_egress_poll_interval_default_is_0() {
        let map = build_lookup();
        let def = map["EgressPollInterval"];
        assert_eq!(def.default, "0");
        // Must be F64 because the node uses fractional seconds.
        assert!(matches!(def.param_type, ParamType::F64 { .. }));
    }

    #[test]
    fn test_chain_sync_idle_timeout_is_f64() {
        let map = build_lookup();
        let def = map["ChainSyncIdleTimeout"];
        assert!(matches!(def.param_type, ParamType::F64 { .. }));
    }

    // ── New params present in schema ───────────────────────────────────────

    #[test]
    fn test_log_directive_param_exists() {
        let map = build_lookup();
        assert!(
            map.contains_key("LogDirective"),
            "LogDirective must be in schema"
        );
        let def = map["LogDirective"];
        assert_eq!(def.section, "Logging");
        assert_eq!(def.param_type, ParamType::String);
    }

    #[test]
    fn test_accepted_connections_limit_param_exists() {
        let map = build_lookup();
        assert!(
            map.contains_key("AcceptedConnectionsLimit"),
            "AcceptedConnectionsLimit must be in schema"
        );
        let def = map["AcceptedConnectionsLimit"];
        assert_eq!(def.section, "Diffusion");
        assert_eq!(def.param_type, ParamType::Object { fields: &[] });
    }

    #[test]
    fn test_storage_param_exists() {
        let map = build_lookup();
        assert!(map.contains_key("Storage"), "Storage must be in schema");
        let def = map["Storage"];
        assert_eq!(def.section, "Storage");
        assert_eq!(def.param_type, ParamType::Object { fields: &[] });
    }

    // ── network_defaults correctness ───────────────────────────────────────

    #[test]
    fn test_network_defaults_mainnet_magic() {
        let map = network_defaults(Network::Mainnet);
        assert_eq!(map["NetworkMagic"], serde_json::json!(764_824_073_u64));
        assert_eq!(
            map["RequiresNetworkMagic"],
            serde_json::json!("RequiresNoMagic")
        );
        assert_eq!(map["Network"], serde_json::json!("Mainnet"));
    }

    #[test]
    fn test_network_defaults_preview_magic() {
        let map = network_defaults(Network::Preview);
        assert_eq!(map["NetworkMagic"], serde_json::json!(2_u64));
        assert_eq!(
            map["RequiresNetworkMagic"],
            serde_json::json!("RequiresMagic")
        );
        assert_eq!(map["Network"], serde_json::json!("Testnet"));
    }

    #[test]
    fn test_network_defaults_genesis_paths() {
        let map = network_defaults(Network::Preview);
        assert_eq!(
            map["ByronGenesisFile"],
            serde_json::json!("preview-byron-genesis.json")
        );
        assert_eq!(
            map["ConwayGenesisFile"],
            serde_json::json!("preview-conway-genesis.json")
        );
        assert_eq!(
            map["DijkstraGenesisFile"],
            serde_json::json!("preview-dijkstra-genesis.json")
        );
        assert_eq!(map["DijkstraGenesisHash"], serde_json::json!(""));
    }

    #[test]
    fn test_network_defaults_peer_targets_match_node() {
        // Verify the network_defaults peer targets match NodeConfig defaults.
        let map = network_defaults(Network::Preview);
        assert_eq!(map["TargetNumberOfActivePeers"], serde_json::json!(20));
        assert_eq!(map["TargetNumberOfEstablishedPeers"], serde_json::json!(30));
        assert_eq!(map["TargetNumberOfKnownPeers"], serde_json::json!(150));
        assert_eq!(map["TargetNumberOfRootPeers"], serde_json::json!(60));
        assert_eq!(
            map["SyncTargetNumberOfEstablishedBigLedgerPeers"],
            serde_json::json!(40)
        );
        assert_eq!(map["SyncTargetNumberOfActivePeers"], serde_json::json!(5));
        assert_eq!(
            map["SyncTargetNumberOfEstablishedPeers"],
            serde_json::json!(10)
        );
        assert_eq!(map["SyncTargetNumberOfKnownPeers"], serde_json::json!(150));
    }

    #[test]
    fn test_network_defaults_egress_poll_interval_is_0() {
        let map = network_defaults(Network::Mainnet);
        assert_eq!(map["EgressPollInterval"], serde_json::json!(0));
    }

    #[test]
    fn test_network_defaults_no_peer_sharing_key() {
        // PeerSharing is intentionally omitted from defaults to use auto-detection.
        let map = network_defaults(Network::Mainnet);
        assert!(
            !map.contains_key("PeerSharing"),
            "PeerSharing should not appear in network_defaults (auto-detected)"
        );
    }

    #[test]
    fn test_network_from_str() {
        assert_eq!(Network::from_str("mainnet"), Some(Network::Mainnet));
        assert_eq!(Network::from_str("PREVIEW"), Some(Network::Preview));
        assert_eq!(Network::from_str("preprod"), Some(Network::Preprod));
        assert_eq!(Network::from_str("devnet"), None);
    }

    // ── Roundtrip: network_defaults → JSON → NodeConfig ───────────────────
    // This verifies that every field produced by network_defaults() survives a
    // round-trip through dugite-node's NodeConfig deserializer without
    // type-mismatch or field-loss errors.  We only check a representative
    // subset because NodeConfig lives in a different crate and we do not import
    // it here; the full roundtrip test lives in config_coverage.rs.

    #[test]
    fn test_network_defaults_produce_valid_json() {
        let map = network_defaults(Network::Preview);
        let json = serde_json::Value::Object(map.clone());
        // Must round-trip to/from JSON without error.
        let s = serde_json::to_string(&json).expect("serialise");
        let reparsed: serde_json::Value = serde_json::from_str(&s).expect("parse");
        assert_eq!(reparsed, json);
    }

    #[test]
    fn test_known_params_count() {
        // We expect at least 45 known parameters (audit baseline: 43 before
        // this change; +3 new: LogDirective, AcceptedConnectionsLimit, Storage).
        assert!(
            KNOWN_PARAMS.len() >= 45,
            "Expected >= 45 known params, got {}",
            KNOWN_PARAMS.len()
        );
    }

    #[test]
    fn test_every_known_param_has_unique_key() {
        let mut seen = std::collections::HashSet::new();
        for def in KNOWN_PARAMS {
            assert!(
                seen.insert(def.key),
                "Duplicate key '{}' in KNOWN_PARAMS",
                def.key
            );
        }
    }

    #[test]
    fn test_every_known_param_has_valid_section() {
        let valid_sections: std::collections::HashSet<&str> =
            SECTION_ORDER.iter().copied().collect();
        for def in KNOWN_PARAMS {
            assert!(
                valid_sections.contains(def.section),
                "ParamDef '{}' has unknown section '{}'",
                def.key,
                def.section
            );
        }
    }

    #[test]
    fn test_subparam_default_as_json_recurses() {
        // Build a nested sub-schema by hand: outer { "x": u64=1, "inner": object { "y": bool=true } }.
        const INNER: &[SubParamDef] = &[SubParamDef {
            key: "y",
            param_type: ParamType::Bool,
            default: "true",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        }];
        const OUTER: &[SubParamDef] = &[
            SubParamDef {
                key: "x",
                param_type: ParamType::U64 { min: 0, max: 10 },
                default: "1",
                description: "",
                tuning_hint: "",
                reloadability: Reloadability::Restart,
            },
            SubParamDef {
                key: "inner",
                param_type: ParamType::Object { fields: INNER },
                default: "",
                description: "",
                tuning_hint: "",
                reloadability: Reloadability::Restart,
            },
        ];

        let outer = ParamDef {
            key: "Outer",
            section: "Test",
            param_type: ParamType::Object { fields: OUTER },
            default: "",
            description: "",
            tuning_hint: "",
            reloadability: Reloadability::Restart,
        };

        let v = outer.default_as_json().expect("object default");
        let obj = v.as_object().expect("object");
        assert_eq!(obj["x"], serde_json::json!(1));
        assert_eq!(obj["inner"], serde_json::json!({ "y": true }));
    }
}
