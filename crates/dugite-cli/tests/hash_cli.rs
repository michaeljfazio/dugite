//! Golden + negative tests for the `hash` command group (#1008 — cardano-cli
//! surface-parity backlog).
//!
//! `hash anchor-data` / `hash script` / `hash genesis-file` are new in this
//! change. Every positive-path expected value below was captured by running
//! the SAME input through a real `cardano-cli 11.0.0.0` binary
//! (`97036a66bcf8c89f687ae57a048eecc0389977ef`) side-by-side with
//! `dugite-cli` and diffing byte-for-byte — see the shell transcript in the
//! PR description / issue comment for the exact invocations. Pinning the
//! result here (rather than shelling out to `cardano-cli` at test time)
//! matches this crate's existing pattern (`command_files.rs`,
//! `output_golden.rs`): CI doesn't need cardano-cli installed to catch a
//! regression.
//!
//! The `hash script` Plutus-envelope cases additionally cross-check against
//! `tests/conformance/upstream/plutus-examples.json` — script bytes AND
//! their hashes are both vendored straight from cardano-ledger's own
//! `Test.Cardano.Ledger.Plutus.Examples`, so this is a second, independent
//! oracle beyond the interactive cardano-cli run.
//!
//! Standing caveat (#951-class): a same-process round trip proves nothing
//! about parity with the real thing. These tests exist specifically because
//! that caveat does NOT apply here — every positive value was checked
//! against actual cardano-cli output, not derived from dugite's own encoder.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_dugite-cli");

fn run(args: &[&str]) -> std::process::Output {
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

fn run_ok_stdout(args: &[&str]) -> String {
    let out = run(args);
    assert!(
        out.status.success(),
        "{:?} failed: stderr={}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("non-utf8 stdout")
}

fn assert_fails(args: &[&str]) {
    let out = run(args);
    assert!(
        !out.status.success(),
        "{args:?} must exit nonzero, got stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
}

fn write_tmp(dir: &Path, name: &str, content: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, content).unwrap();
    p
}

// ── hash anchor-data ────────────────────────────────────────────────────

/// `cardano-cli hash anchor-data --text "hello"` -> the hex below, with NO
/// trailing newline (verified with `xxd`: real cardano-cli does not print
/// one for `anchor-data`/`script`, unlike `genesis-file` which does — two
/// genuinely different upstream code paths).
const HELLO_ANCHOR_HASH: &str = "324dcf027dd4a30a932c441f365a25e86b173defa4b8e58948253471b81b72cf";

#[test]
fn anchor_data_text_matches_cardano_cli_golden() {
    let out = run_ok_stdout(&["hash", "anchor-data", "--text", "hello"]);
    assert_eq!(
        out, HELLO_ANCHOR_HASH,
        "no trailing newline expected either"
    );
}

#[test]
fn anchor_data_file_binary_and_text_agree_with_text_flag() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "anchor.txt", "hello");
    let out = run_ok_stdout(&["hash", "anchor-data", "--file-binary", f.to_str().unwrap()]);
    assert_eq!(out, HELLO_ANCHOR_HASH);

    let out2 = run_ok_stdout(&["hash", "anchor-data", "--file-text", f.to_str().unwrap()]);
    assert_eq!(out2, HELLO_ANCHOR_HASH);
}

#[test]
fn anchor_data_out_file_has_no_trailing_newline() {
    let dir = tempfile::tempdir().unwrap();
    let out_file = dir.path().join("hash.out");
    let out = run(&[
        "hash",
        "anchor-data",
        "--text",
        "hello",
        "--out-file",
        out_file.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let written = std::fs::read(&out_file).unwrap();
    assert_eq!(written, HELLO_ANCHOR_HASH.as_bytes());
}

#[test]
fn anchor_data_expected_hash_match_prints_confirmation() {
    let out = run_ok_stdout(&[
        "hash",
        "anchor-data",
        "--text",
        "hello",
        "--expected-hash",
        HELLO_ANCHOR_HASH,
    ]);
    // cardano-cli prints exactly "Hashes match!\n" on stdout, exit 0.
    assert_eq!(out.trim(), "Hashes match!");
}

#[test]
fn anchor_data_expected_hash_mismatch_fails_nonzero() {
    let bogus = "0".repeat(64);
    assert_fails(&[
        "hash",
        "anchor-data",
        "--text",
        "hello",
        "--expected-hash",
        &bogus,
    ]);
}

#[test]
fn anchor_data_no_source_flag_is_rejected() {
    assert_fails(&["hash", "anchor-data"]);
}

#[test]
fn anchor_data_mutually_exclusive_flags_rejected_by_clap() {
    assert_fails(&[
        "hash",
        "anchor-data",
        "--text",
        "hi",
        "--file-binary",
        "/nonexistent",
    ]);
    assert_fails(&[
        "hash",
        "anchor-data",
        "--text",
        "hi",
        "--expected-hash",
        &"0".repeat(64),
        "--out-file",
        "/tmp/should-not-be-created-1008.txt",
    ]);
}

#[test]
fn anchor_data_ipfs_url_rejected_not_silently_treated_as_http() {
    // No network dependency: this must fail BEFORE any fetch is attempted.
    let stderr = {
        let out = run(&["hash", "anchor-data", "--url", "ipfs://bafy1234"]);
        assert!(!out.status.success());
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(
        stderr.to_lowercase().contains("ipfs"),
        "expected an explicit IPFS-not-supported error, got: {stderr}"
    );
}

// ── hash script (native) ────────────────────────────────────────────────

/// `{"type":"sig","keyHash":"c6ffca9b32e97ecbdc22aab0b40cca80d8f22e2f22fe7c78f2fe95d3"}`
/// hashed via real cardano-cli 11.0.0.0 `hash script`.
const SIG_SCRIPT_JSON: &str =
    r#"{"type":"sig","keyHash":"c6ffca9b32e97ecbdc22aab0b40cca80d8f22e2f22fe7c78f2fe95d3"}"#;
const SIG_SCRIPT_HASH: &str = "0d86935fbede2aaadb9070781a1899a4529b0574d8812b21241b4449";

#[test]
fn hash_script_native_sig_matches_cardano_cli_golden() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "sig.json", SIG_SCRIPT_JSON);
    let out = run_ok_stdout(&["hash", "script", "--script-file", f.to_str().unwrap()]);
    assert_eq!(out, SIG_SCRIPT_HASH);
}

/// `hash script` and `transaction policyid` must agree byte-for-byte on the
/// same native script — they are documented as sharing one parser
/// (`parse_json_native_script`), and this is the regression test for that
/// contract: if a future edit forks the two paths, this catches it without
/// needing cardano-cli at all.
#[test]
fn hash_script_native_agrees_with_transaction_policyid() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "sig.json", SIG_SCRIPT_JSON);
    let hash_out = run_ok_stdout(&["hash", "script", "--script-file", f.to_str().unwrap()]);
    let policyid_out = run_ok_stdout(&[
        "transaction",
        "policyid",
        "--script-file",
        f.to_str().unwrap(),
    ]);
    assert_eq!(hash_out, policyid_out.trim());
}

#[test]
fn hash_script_out_file_matches_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "sig.json", SIG_SCRIPT_JSON);
    let out_file = dir.path().join("script-hash.out");
    let out = run(&[
        "hash",
        "script",
        "--script-file",
        f.to_str().unwrap(),
        "--out-file",
        out_file.to_str().unwrap(),
    ]);
    assert!(out.status.success());
    let written = std::fs::read_to_string(&out_file).unwrap();
    assert_eq!(written, SIG_SCRIPT_HASH);
}

// ── hash script (Plutus V1/V2/V3) ───────────────────────────────────────
//
// script_hex values and their expected script_hash are vendored VERBATIM
// from tests/conformance/upstream/plutus-examples.json (cardano-ledger's own
// Test.Cardano.Ledger.Plutus.Examples, #969/#970) — `alwaysSucceedsNoDatum`,
// the first entry. Cross-checked independently against real cardano-cli
// (see module doc). script_hex already carries ONE CBOR byte-string wrapper
// (cardano-api's `cborHex` for a Plutus script is a deliberate identity over
// that wrapped form — oracle-verified against
// cardano-ledger-core/.../Plutus/Language.hs and empirically against a real
// mainnet script hash); do NOT "simplify" this by stripping a header.
const PLUTUS_SCRIPT_HEX: &str =
    "582d01000033333222222253330053370e900118039baa30033006300437540022c224002aae755d12b9a5573cae85";

fn plutus_envelope(version_type: &str) -> String {
    format!(r#"{{"type":"{version_type}","description":"","cborHex":"{PLUTUS_SCRIPT_HEX}"}}"#)
}

#[test]
fn hash_script_plutus_v1_matches_ledger_fixture_and_cardano_cli() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "v1.json", &plutus_envelope("PlutusScriptV1"));
    let out = run_ok_stdout(&["hash", "script", "--script-file", f.to_str().unwrap()]);
    assert_eq!(
        out,
        "6bd534d263a1213113b775e4e8386e47e6181a33e40ab3ea623b5fe8"
    );
}

#[test]
fn hash_script_plutus_v2_matches_ledger_fixture_and_cardano_cli() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "v2.json", &plutus_envelope("PlutusScriptV2"));
    let out = run_ok_stdout(&["hash", "script", "--script-file", f.to_str().unwrap()]);
    assert_eq!(
        out,
        "a98c0f498abacf6dea126d707b1ba5cc27e523c20929ec0ac705087f"
    );
}

#[test]
fn hash_script_plutus_v3_matches_ledger_fixture_and_cardano_cli() {
    // Different script body for V3 (the fixture's V3 arm compiles
    // differently), still from the SAME upstream JSON, still cross-checked
    // against real cardano-cli interactively.
    let v3_hex = "588f0101009800aab9daba2ab9aaab9eaba1ab9c488888896600264b30013370e90011804000c4c8c8ca4d6600266e1d2000002894004c02000515980099b874800800a250028b2012402400324a14a23007002300900137546008600e600a00314a28030dd5191919191803980500198030011802801180380098021baa0018a4d13263300249010350543500800200a1";
    let json = format!(r#"{{"type":"PlutusScriptV3","description":"","cborHex":"{v3_hex}"}}"#);
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "v3.json", &json);
    let out = run_ok_stdout(&["hash", "script", "--script-file", f.to_str().unwrap()]);
    assert_eq!(
        out,
        "b1d5bc8ced627156f403786ad7c281dcc510735957aa364fb9376d85"
    );
}

#[test]
fn hash_script_rejects_malformed_json() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "bad.json", "not json");
    assert_fails(&["hash", "script", "--script-file", f.to_str().unwrap()]);
}

#[test]
fn hash_script_rejects_missing_file() {
    assert_fails(&["hash", "script", "--script-file", "/does/not/exist.json"]);
}

#[test]
fn hash_script_rejects_plutus_envelope_missing_cbor_hex() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(
        dir.path(),
        "bad-plutus.json",
        r#"{"type":"PlutusScriptV2","description":""}"#,
    );
    assert_fails(&["hash", "script", "--script-file", f.to_str().unwrap()]);
}

#[test]
fn hash_script_rejects_unknown_native_script_type() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "bad-native.json", r#"{"type":"bogus"}"#);
    assert_fails(&["hash", "script", "--script-file", f.to_str().unwrap()]);
}

// ── governance drep metadata-hash ───────────────────────────────────────
//
// Shares `fetch_url_bytes` and the `blake2b_256(raw bytes)` computation with
// `hash anchor-data` — golden value captured from a real cardano-cli
// `conway governance drep metadata-hash` run on the same fixture bytes.

const DREP_META_FIXTURE: &str = r#"{"a":1}"#;
const DREP_META_HASH: &str = "10a7ff3e312baec0c356be489739b93f63af84416c40f1c13023eb96c7ed50aa";

#[test]
fn governance_drep_metadata_hash_matches_cardano_cli_golden() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "drep-meta.json", DREP_META_FIXTURE);
    let out = run_ok_stdout(&[
        "governance",
        "drep",
        "metadata-hash",
        "--drep-metadata-file",
        f.to_str().unwrap(),
    ]);
    assert_eq!(out, DREP_META_HASH, "no trailing newline expected either");
}

#[test]
fn governance_drep_metadata_hash_expected_hash_match() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "drep-meta.json", DREP_META_FIXTURE);
    let out = run_ok_stdout(&[
        "governance",
        "drep",
        "metadata-hash",
        "--drep-metadata-file",
        f.to_str().unwrap(),
        "--expected-hash",
        DREP_META_HASH,
    ]);
    assert_eq!(out.trim(), "Hashes match!");
}

#[test]
fn governance_drep_metadata_hash_no_source_flag_is_rejected() {
    assert_fails(&["governance", "drep", "metadata-hash"]);
}

#[test]
fn governance_drep_metadata_hash_mutually_exclusive_flags_rejected() {
    assert_fails(&[
        "governance",
        "drep",
        "metadata-hash",
        "--drep-metadata-file",
        "/nonexistent",
        "--drep-metadata-url",
        "http://example.invalid/x",
    ]);
}

// ── hash genesis-file ───────────────────────────────────────────────────

/// Same fixture bytes as `genesis.rs`'s
/// `test_genesis_hash_raw_bytes_matches_cardano_cli` (keys deliberately out
/// of alphabetical order, to prove raw-bytes hashing rather than a
/// parse+reserialize round trip that would reorder them) — reused here
/// rather than re-derived, so both tests are provably hashing the same
/// thing the same way.
const GENESIS_FIXTURE: &str = r#"{"z":99,"a":1}"#;
const GENESIS_FIXTURE_HASH: &str =
    "1a82d5ea4a94dc561407f739963678a495d0638f75e38da5eb9d0232b2e0b697";

#[test]
fn hash_genesis_file_matches_genesis_hash_fixture() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "genesis.json", GENESIS_FIXTURE);
    let out = run_ok_stdout(&["hash", "genesis-file", "--genesis", f.to_str().unwrap()]);
    // Unlike anchor-data/script, cardano-cli DOES print a trailing newline
    // for hash genesis-file (verified with `xxd` on real cardano-cli).
    assert_eq!(out, format!("{GENESIS_FIXTURE_HASH}\n"));
}

/// `genesis hash` (deprecated alias upstream) and `hash genesis-file` (the
/// new canonical name) must produce byte-identical output — same fixture,
/// same underlying blake2b_256(raw bytes) computation.
#[test]
fn hash_genesis_file_agrees_with_legacy_genesis_hash() {
    let dir = tempfile::tempdir().unwrap();
    let f = write_tmp(dir.path(), "genesis.json", GENESIS_FIXTURE);
    let new_out = run_ok_stdout(&["hash", "genesis-file", "--genesis", f.to_str().unwrap()]);
    let legacy_out = run_ok_stdout(&["genesis", "hash", "--genesis-file", f.to_str().unwrap()]);
    assert_eq!(new_out.trim(), legacy_out.trim());
}

#[test]
fn hash_genesis_file_rejects_missing_file() {
    assert_fails(&["hash", "genesis-file", "--genesis", "/does/not/exist.json"]);
}

// ── version ──────────────────────────────────────────────────────────────

#[test]
fn version_subcommand_matches_flag_version() {
    let via_subcommand = run_ok_stdout(&["version"]);
    let via_flag = run_ok_stdout(&["--version"]);
    assert_eq!(via_subcommand.trim(), via_flag.trim());
    assert!(via_subcommand.starts_with("dugite-cli "));
}

// ── stake-pool deregistration-certificate / stake-address
//    stake-delegation-certificate: cardano-cli-name aliasing (#1008) ──────

#[test]
fn stake_pool_deregistration_certificate_is_now_the_primary_name() {
    let out = run_ok_stdout(&["stake-pool", "--help"]);
    assert!(
        out.contains("deregistration-certificate"),
        "cardano-cli's canonical name must be discoverable in --help:\n{out}"
    );
}

#[test]
fn stake_address_stake_delegation_certificate_is_now_the_primary_name() {
    let out = run_ok_stdout(&["stake-address", "--help"]);
    assert!(
        out.contains("stake-delegation-certificate"),
        "cardano-cli's canonical name must be discoverable in --help:\n{out}"
    );
}

// ── query tx-mempool tx-exists rename (#1008) ───────────────────────────

#[test]
fn tx_mempool_help_mentions_tx_exists_not_only_has_tx() {
    let out = run_ok_stdout(&["query", "tx-mempool", "--help"]);
    assert!(
        out.contains("tx-exists"),
        "cardano-cli's exact subcommand vocabulary must appear:\n{out}"
    );
}
