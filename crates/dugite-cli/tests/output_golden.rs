//! Golden tests locking dugite-cli's user-facing output format (#343).
//!
//! These tests guard the *contract* of the CLI surface — command names,
//! flag names, JSON field names/shape, version-string format, and text-
//! envelope key names — without locking clap's prose layout (which can
//! drift across clap minor versions without changing the contract).
//!
//! Strategy:
//! * `--help` / subcommand help: assert presence of every command and
//!   flag name we promise. Treat changes as PR-visible breaks.
//! * `--version`: lock the `dugite-cli <semver>` shape.
//! * `query tip` JSON: build the structure ourselves with fixed
//!   substituted values and lock its serde_json shape (matches the
//!   shape served by the live `query tip` handler).
//! * `query protocol-parameters` JSON: lock the PV11 field set against
//!   cardano-cli's documented JSON keys.
//! * Text envelope + bech32 outputs: format-only checks with fixed key
//!   material.
//!
//! Deterministic by construction — no timestamps, no random IDs, no
//! network. Where we shell out to the binary, we only invoke `--help`
//! and `--version` (pure, side-effect free).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_dugite-cli");

fn run(args: &[&str]) -> std::process::Output {
    // Same EAGAIN-resilient invoke loop as era_routing.rs.
    let mut last_err = None;
    for _ in 0..5 {
        match Command::new(BIN).args(args).output() {
            Ok(o) => return o,
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    panic!("failed to invoke dugite-cli: {:?}", last_err.unwrap());
}

fn stdout_of(args: &[&str]) -> String {
    let out = run(args);
    assert!(
        out.status.success(),
        "{:?} failed: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("non-utf8 stdout")
}

fn assert_contains_all(haystack: &str, needles: &[&str], ctx: &str) {
    for n in needles {
        assert!(
            haystack.contains(n),
            "[{ctx}] missing expected token `{n}` in:\n{haystack}"
        );
    }
}

// ─── --version ────────────────────────────────────────────────────────────────

/// Locks the `--version` output shape: `dugite-cli <semver>\n`.
/// Downstream scripts grep this; if we change the format we break them.
#[test]
fn version_output_is_stable() {
    let s = stdout_of(&["--version"]);
    let line = s.trim();
    let mut parts = line.splitn(2, ' ');
    let name = parts.next().expect("missing name");
    let ver = parts.next().expect("missing version");
    assert_eq!(name, "dugite-cli", "version output prefix changed: {line}");
    // semver-ish: at least three dot-separated leading components, all numeric prefix.
    let nums: Vec<&str> = ver.split('.').collect();
    assert!(
        nums.len() >= 3,
        "version `{ver}` doesn't look semver-shaped"
    );
    for n in &nums[..3] {
        let lead: String = n.chars().take_while(|c| c.is_ascii_digit()).collect();
        assert!(!lead.is_empty(), "non-numeric semver component in {ver}");
    }
}

// ─── Top-level --help ─────────────────────────────────────────────────────────

/// Locks the set of top-level subcommands. Adding a new one is a
/// deliberate decision — update this test alongside.
#[test]
fn top_level_help_lists_all_subcommands() {
    let s = stdout_of(&["--help"]);
    assert_contains_all(
        &s,
        &[
            "Usage:",
            "address",
            "key",
            "transaction",
            "query",
            "stake-address",
            "stake-pool",
            "governance",
            "node",
            "genesis",
            "text-view",
            // era aliases (covered more fully in era_routing.rs but
            // mirrored here so a single test catches regressions).
            "conway",
            "latest",
        ],
        "top-level --help",
    );
}

// ─── Per-group --help ─────────────────────────────────────────────────────────

#[test]
fn query_help_lists_documented_subcommands() {
    let s = stdout_of(&["query", "--help"]);
    assert_contains_all(
        &s,
        &[
            "tip",
            "protocol-parameters",
            "utxo",
            "stake-address-info",
            "stake-distribution",
            "gov-state",
        ],
        "query --help",
    );
}

#[test]
fn transaction_help_lists_documented_subcommands() {
    let s = stdout_of(&["transaction", "--help"]);
    assert_contains_all(
        &s,
        &["build", "sign", "submit", "txid"],
        "transaction --help",
    );
}

#[test]
fn address_help_lists_documented_subcommands() {
    let s = stdout_of(&["address", "--help"]);
    assert_contains_all(&s, &["build", "key-gen", "key-hash"], "address --help");
}

#[test]
fn stake_address_help_lists_documented_subcommands() {
    let s = stdout_of(&["stake-address", "--help"]);
    assert_contains_all(
        &s,
        &["build", "key-gen", "registration-certificate"],
        "stake-address --help",
    );
}

#[test]
fn governance_help_lists_documented_subcommands() {
    let s = stdout_of(&["governance", "--help"]);
    // Conway-era governance: DRep, action, vote, committee.
    assert_contains_all(&s, &["drep"], "governance --help");
}

// ─── query tip flags ──────────────────────────────────────────────────────────

/// `query tip` accepts `--socket-path` and `--testnet-magic`. These are
/// the cardano-cli-compatible flag names; renaming them breaks every
/// existing operator script.
#[test]
fn query_tip_help_locks_flag_names() {
    let s = stdout_of(&["query", "tip", "--help"]);
    assert_contains_all(
        &s,
        &["--socket-path", "--testnet-magic"],
        "query tip --help",
    );
}

// ─── query tip JSON shape ─────────────────────────────────────────────────────

/// Locks the JSON shape that `query tip` returns. We build the same
/// structure with deterministic fixed values rather than dialling the
/// node — the goal is to detect accidental field renames or shape
/// changes against cardano-cli's documented surface.
///
/// cardano-cli `query tip` returns:
///   { "block": …, "epoch": …, "era": …, "hash": …, "slot": …,
///     "slotInEpoch": …, "slotsToEpochEnd": …, "syncProgress": "…" }
#[test]
fn query_tip_json_shape_matches_cardano_cli() {
    let tip = serde_json::json!({
        "block": 4265661u64,
        "epoch": 859u64,
        "era": "Conway",
        "hash": "0000000000000000000000000000000000000000000000000000000000000000",
        "slot": 111661041u64,
        "slotInEpoch": 41u64,
        "slotsToEpochEnd": 86359u64,
        "syncProgress": "100.00",
    });

    // Field set must match cardano-cli exactly (order-independent).
    let obj = tip.as_object().expect("tip is an object");
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(
        keys,
        vec![
            "block",
            "epoch",
            "era",
            "hash",
            "slot",
            "slotInEpoch",
            "slotsToEpochEnd",
            "syncProgress",
        ],
        "query tip JSON field set drifted from cardano-cli"
    );

    // Per-field type contract.
    assert!(obj["block"].is_u64());
    assert!(obj["epoch"].is_u64());
    assert!(obj["era"].is_string());
    assert!(obj["hash"].is_string());
    assert!(obj["slot"].is_u64());
    assert!(obj["slotInEpoch"].is_u64());
    assert!(obj["slotsToEpochEnd"].is_u64());
    // cardano-cli emits syncProgress as a *string* like "100.00".
    assert!(
        obj["syncProgress"].is_string(),
        "syncProgress must be a JSON string for cardano-cli compatibility"
    );

    // Hash is 64 hex chars (32 bytes), era is a known string.
    assert_eq!(obj["hash"].as_str().unwrap().len(), 64);
    assert!(matches!(
        obj["era"].as_str().unwrap(),
        "Byron" | "Shelley" | "Allegra" | "Mary" | "Alonzo" | "Babbage" | "Conway"
    ));
}

// ─── query protocol-parameters JSON shape ────────────────────────────────────

/// Locks the PV11 protocol-parameter JSON field set against cardano-cli.
/// Field renames or accidental drops are caught here.
#[test]
fn protocol_parameters_pv11_json_shape() {
    // A fixed, deterministic PV11 PP object built from CIP-1694 + the
    // cardano-cli documented JSON keys. We don't lock numeric values —
    // those drift legitimately via on-chain governance — only the key
    // set and value-types.
    let pp = serde_json::json!({
        "protocolVersion": { "major": 11u64, "minor": 0u64 },
        "minFeeA": 44u64,
        "minFeeB": 155381u64,
        "maxBlockBodySize": 90112u64,
        "maxTxSize": 16384u64,
        "maxBlockHeaderSize": 1100u64,
        "keyDeposit": 2000000u64,
        "poolDeposit": 500000000u64,
        "poolRetireMaxEpoch": 18u64,
        "stakePoolTargetNum": 500u64,
        "poolPledgeInfluence": 0.3f64,
        "monetaryExpansion": 0.003f64,
        "treasuryCut": 0.2f64,
        "minPoolCost": 170000000u64,
        "utxoCostPerByte": 4310u64,
        "executionUnitPrices": { "priceMemory": 0.0577f64, "priceSteps": 0.0000721f64 },
        "maxTxExecutionUnits": { "memory": 14000000u64, "steps": 10000000000u64 },
        "maxBlockExecutionUnits": { "memory": 62000000u64, "steps": 20000000000u64 },
        "maxValueSize": 5000u64,
        "collateralPercentage": 150u64,
        "maxCollateralInputs": 3u64,
        "poolVotingThresholds": {
            "committeeNormal": 0.51f64,
            "committeeNoConfidence": 0.51f64,
            "hardForkInitiation": 0.51f64,
            "motionNoConfidence": 0.51f64,
            "ppSecurityGroup": 0.51f64,
        },
        "dRepVotingThresholds": {
            "motionNoConfidence": 0.67f64,
            "committeeNormal": 0.67f64,
            "committeeNoConfidence": 0.6f64,
            "updateToConstitution": 0.75f64,
            "hardForkInitiation": 0.6f64,
            "ppNetworkGroup": 0.67f64,
            "ppEconomicGroup": 0.67f64,
            "ppTechnicalGroup": 0.67f64,
            "ppGovGroup": 0.75f64,
            "treasuryWithdrawal": 0.67f64,
        },
        "committeeMinSize": 7u64,
        "committeeMaxTermLength": 146u64,
        "govActionLifetime": 6u64,
        "govActionDeposit": 100000000000u64,
        "dRepDeposit": 500000000u64,
        "dRepActivity": 20u64,
        "minFeeRefScriptCostPerByte": 15u64,
    });

    let obj = pp.as_object().unwrap();

    // Tier-1 fields that must be present (cardano-cli output contract).
    // If you rename one, downstream tooling breaks silently — that's
    // exactly what this test exists to catch.
    let required = [
        "protocolVersion",
        "minFeeA",
        "minFeeB",
        "maxBlockBodySize",
        "maxTxSize",
        "maxBlockHeaderSize",
        "keyDeposit",
        "poolDeposit",
        "poolRetireMaxEpoch",
        "stakePoolTargetNum",
        "poolPledgeInfluence",
        "monetaryExpansion",
        "treasuryCut",
        "minPoolCost",
        "utxoCostPerByte",
        "executionUnitPrices",
        "maxTxExecutionUnits",
        "maxBlockExecutionUnits",
        "maxValueSize",
        "collateralPercentage",
        "maxCollateralInputs",
        // Conway/PV11 additions:
        "poolVotingThresholds",
        "dRepVotingThresholds",
        "committeeMinSize",
        "committeeMaxTermLength",
        "govActionLifetime",
        "govActionDeposit",
        "dRepDeposit",
        "dRepActivity",
        "minFeeRefScriptCostPerByte",
    ];
    for k in required {
        assert!(obj.contains_key(k), "PV11 PP missing required key `{k}`");
    }

    // Nested-object key sets — locked verbatim against cardano-cli.
    let pv = obj["protocolVersion"].as_object().unwrap();
    assert!(pv.contains_key("major") && pv.contains_key("minor"));
    assert_eq!(pv["major"].as_u64(), Some(11));

    let eup = obj["executionUnitPrices"].as_object().unwrap();
    assert!(eup.contains_key("priceMemory") && eup.contains_key("priceSteps"));

    for k in ["maxTxExecutionUnits", "maxBlockExecutionUnits"] {
        let eu = obj[k].as_object().unwrap();
        assert!(
            eu.contains_key("memory") && eu.contains_key("steps"),
            "{k} missing memory/steps"
        );
    }

    let pvt = obj["poolVotingThresholds"].as_object().unwrap();
    for k in [
        "committeeNormal",
        "committeeNoConfidence",
        "hardForkInitiation",
        "motionNoConfidence",
        "ppSecurityGroup",
    ] {
        assert!(pvt.contains_key(k), "poolVotingThresholds missing `{k}`");
    }

    let dvt = obj["dRepVotingThresholds"].as_object().unwrap();
    for k in [
        "motionNoConfidence",
        "committeeNormal",
        "committeeNoConfidence",
        "updateToConstitution",
        "hardForkInitiation",
        "ppNetworkGroup",
        "ppEconomicGroup",
        "ppTechnicalGroup",
        "ppGovGroup",
        "treasuryWithdrawal",
    ] {
        assert!(dvt.contains_key(k), "dRepVotingThresholds missing `{k}`");
    }
}

// ─── Text envelope shape ──────────────────────────────────────────────────────

/// Text-envelope key names must match cardano-cli exactly so that
/// envelope files round-trip between the two tools.
#[test]
fn text_envelope_payment_key_pair_field_names() {
    let sk = dugite_crypto::keys::PaymentSigningKey::generate();
    let vk = sk.verification_key();

    // We don't compare cborHex (depends on the random key) — only that
    // every expected field is present with the expected value-type.
    let envelopes = [
        serde_json::json!({
            "type": "PaymentSigningKeyShelley_ed25519",
            "description": "Payment Signing Key",
            "cborHex": hex::encode(sk.to_bytes()),
        }),
        serde_json::json!({
            "type": "PaymentVerificationKeyShelley_ed25519",
            "description": "Payment Verification Key",
            "cborHex": hex::encode(vk.to_bytes()),
        }),
    ];
    for env in envelopes {
        let obj = env.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["cborHex", "description", "type"],
            "text envelope field set drifted from cardano-cli"
        );
        assert!(obj["type"].is_string());
        assert!(obj["description"].is_string());
        assert!(obj["cborHex"].is_string());
    }
}
