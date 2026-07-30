//! End-to-end file-based command tests.
//!
//! Invokes the `dugite-cli` binary (no node, no socket, no network) for the
//! offline key/address/node command surface and verifies the *contents* of
//! what it writes and prints: text-envelope structure, key correspondence,
//! bech32 address shape, hash values, and counter CBOR. This covers the clap
//! flag wiring that inline unit tests (which construct subcommands directly)
//! cannot reach.
//!
//! Error-path coverage: missing files, malformed envelopes, and invalid
//! bech32 must exit nonzero — silent success on bad input is exactly the
//! failure shape this project treats as a defect.

use std::path::Path;
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

fn run_ok(args: &[&str]) -> String {
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

/// Read a text-envelope JSON file, returning the parsed value.
fn read_envelope(path: &Path) -> serde_json::Value {
    let content = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
}

/// Decode a 0x5820-prefixed cborHex into the raw 32 key bytes.
fn raw32_from_envelope(env: &serde_json::Value) -> [u8; 32] {
    let cbor = hex::decode(env["cborHex"].as_str().expect("cborHex")).unwrap();
    assert_eq!(&cbor[..2], &[0x58, 0x20], "expected CBOR bytes(32) header");
    cbor[2..].try_into().unwrap()
}

// ── key ──────────────────────────────────────────────────────────────────────

#[test]
fn key_generate_payment_key_and_hash_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let skey = dir.path().join("payment.skey");
    let vkey = dir.path().join("payment.vkey");

    run_ok(&[
        "key",
        "generate-payment-key",
        "--signing-key-file",
        skey.to_str().unwrap(),
        "--verification-key-file",
        vkey.to_str().unwrap(),
    ]);

    let sk_env = read_envelope(&skey);
    let vk_env = read_envelope(&vkey);
    assert_eq!(sk_env["type"], "PaymentSigningKeyShelley_ed25519");
    assert_eq!(vk_env["type"], "PaymentVerificationKeyShelley_ed25519");

    // The written pair must correspond: vkey derives from skey.
    let sk =
        dugite_crypto::keys::PaymentSigningKey::from_bytes(&raw32_from_envelope(&sk_env)).unwrap();
    let vk_bytes = raw32_from_envelope(&vk_env);
    assert_eq!(sk.verification_key().to_bytes(), vk_bytes);

    // verification-key-hash must print the blake2b-224 of the key in the file.
    let printed = run_ok(&[
        "key",
        "verification-key-hash",
        "--verification-key-file",
        vkey.to_str().unwrap(),
    ]);
    let printed = printed.trim();
    let expected = dugite_crypto::keys::PaymentVerificationKey::from_bytes(&vk_bytes)
        .unwrap()
        .hash()
        .to_hex();
    assert_eq!(printed, expected, "printed hash must match the key on disk");
    assert_eq!(printed.len(), 56, "28-byte hash = 56 hex chars");
}

#[test]
fn key_generate_stake_key_types() {
    let dir = tempfile::tempdir().unwrap();
    let skey = dir.path().join("stake.skey");
    let vkey = dir.path().join("stake.vkey");

    run_ok(&[
        "key",
        "generate-stake-key",
        "--signing-key-file",
        skey.to_str().unwrap(),
        "--verification-key-file",
        vkey.to_str().unwrap(),
    ]);

    assert_eq!(
        read_envelope(&skey)["type"],
        "StakeSigningKeyShelley_ed25519"
    );
    assert_eq!(
        read_envelope(&vkey)["type"],
        "StakeVerificationKeyShelley_ed25519"
    );
}

#[test]
fn key_verification_key_hash_rejects_bad_inputs() {
    let dir = tempfile::tempdir().unwrap();

    // Missing file.
    assert_fails(&[
        "key",
        "verification-key-hash",
        "--verification-key-file",
        dir.path().join("missing.vkey").to_str().unwrap(),
    ]);

    // Malformed JSON.
    let bad_json = dir.path().join("bad.vkey");
    std::fs::write(&bad_json, "not json {").unwrap();
    assert_fails(&[
        "key",
        "verification-key-hash",
        "--verification-key-file",
        bad_json.to_str().unwrap(),
    ]);

    // Non-hex cborHex.
    let bad_hex = dir.path().join("badhex.vkey");
    std::fs::write(
        &bad_hex,
        r#"{"type": "PaymentVerificationKeyShelley_ed25519", "description": "", "cborHex": "zzzz"}"#,
    )
    .unwrap();
    assert_fails(&[
        "key",
        "verification-key-hash",
        "--verification-key-file",
        bad_hex.to_str().unwrap(),
    ]);
}

// ── address ──────────────────────────────────────────────────────────────────

#[test]
fn address_key_gen_build_and_info_flow() {
    let dir = tempfile::tempdir().unwrap();
    let vkey = dir.path().join("payment.vkey");
    let skey = dir.path().join("payment.skey");

    run_ok(&[
        "address",
        "key-gen",
        "--verification-key-file",
        vkey.to_str().unwrap(),
        "--signing-key-file",
        skey.to_str().unwrap(),
    ]);

    // key-hash must print the blake2b-224 of the generated key.
    let vk_bytes = raw32_from_envelope(&read_envelope(&vkey));
    let expected_hash = dugite_crypto::keys::PaymentVerificationKey::from_bytes(&vk_bytes)
        .unwrap()
        .hash()
        .to_hex();
    let printed = run_ok(&[
        "address",
        "key-hash",
        "--payment-verification-key-file",
        vkey.to_str().unwrap(),
    ]);
    assert_eq!(printed.trim(), expected_hash);

    // Mainnet enterprise address printed to stdout.
    let addr = run_ok(&[
        "address",
        "build",
        "--payment-verification-key-file",
        vkey.to_str().unwrap(),
        "--network",
        "mainnet",
    ]);
    let addr = addr.trim();
    assert!(addr.starts_with("addr1"), "mainnet HRP, got: {addr}");
    let (_, bytes) = bech32::decode(addr).unwrap();
    assert_eq!(bytes.len(), 29, "enterprise = header + 28-byte credential");
    assert_eq!(bytes[0], 0x61, "type 6 (enterprise) + network 1 (mainnet)");
    // Payment credential in the address must be the key hash printed above.
    assert_eq!(hex::encode(&bytes[1..]), expected_hash);

    // info must accept the address it just built.
    let info = run_ok(&["address", "info", "--address", addr]);
    assert!(info.contains("Type: Enterprise"), "info output: {info}");
    assert!(info.contains("HRP: addr"), "info output: {info}");
}

#[test]
fn address_build_base_testnet_with_out_file() {
    let dir = tempfile::tempdir().unwrap();
    let pay_vkey = dir.path().join("payment.vkey");
    let pay_skey = dir.path().join("payment.skey");
    let stake_vkey = dir.path().join("stake.vkey");
    let stake_skey = dir.path().join("stake.skey");
    let out = dir.path().join("payment.addr");

    run_ok(&[
        "address",
        "key-gen",
        "--verification-key-file",
        pay_vkey.to_str().unwrap(),
        "--signing-key-file",
        pay_skey.to_str().unwrap(),
    ]);
    run_ok(&[
        "key",
        "generate-stake-key",
        "--verification-key-file",
        stake_vkey.to_str().unwrap(),
        "--signing-key-file",
        stake_skey.to_str().unwrap(),
    ]);

    run_ok(&[
        "address",
        "build",
        "--payment-verification-key-file",
        pay_vkey.to_str().unwrap(),
        "--stake-verification-key-file",
        stake_vkey.to_str().unwrap(),
        "--network",
        "testnet",
        "--out-file",
        out.to_str().unwrap(),
    ]);

    let addr = std::fs::read_to_string(&out).unwrap();
    assert!(addr.starts_with("addr_test1"), "testnet HRP, got: {addr}");
    let (_, bytes) = bech32::decode(&addr).unwrap();
    assert_eq!(bytes.len(), 57, "base = header + 28 payment + 28 stake");
    assert_eq!(
        bytes[0], 0x00,
        "type 0 (base key+key) + network 0 (testnet)"
    );

    // The stake part must be the hash of the stake key on disk.
    let stake_bytes = raw32_from_envelope(&read_envelope(&stake_vkey));
    let stake_hash = dugite_crypto::keys::PaymentVerificationKey::from_bytes(&stake_bytes)
        .unwrap()
        .hash();
    assert_eq!(&bytes[29..], &stake_hash.as_bytes()[..]);

    let info = run_ok(&["address", "info", "--address", addr.trim()]);
    assert!(info.contains("Type: Base"), "info output: {info}");
}

#[test]
fn address_info_rejects_invalid_bech32() {
    assert_fails(&["address", "info", "--address", "addr1notbech32!!!"]);
}

#[test]
fn address_build_missing_key_file_fails() {
    let dir = tempfile::tempdir().unwrap();
    assert_fails(&[
        "address",
        "build",
        "--payment-verification-key-file",
        dir.path().join("missing.vkey").to_str().unwrap(),
        "--network",
        "mainnet",
    ]);
}

// ── node ─────────────────────────────────────────────────────────────────────

#[test]
fn node_key_gen_vrf_and_key_hash_vrf_agree() {
    let dir = tempfile::tempdir().unwrap();
    let vkey = dir.path().join("vrf.vkey");
    let skey = dir.path().join("vrf.skey");

    run_ok(&[
        "node",
        "key-gen-vrf",
        "--verification-key-file",
        vkey.to_str().unwrap(),
        "--signing-key-file",
        skey.to_str().unwrap(),
    ]);

    let vk_env = read_envelope(&vkey);
    assert_eq!(vk_env["type"], "VrfVerificationKey_PraosVRF");
    let raw = raw32_from_envelope(&vk_env);

    // key-hash-vrf must print blake2b-256 of the raw key in the file.
    let printed = run_ok(&[
        "node",
        "key-hash-vrf",
        "--verification-key-file",
        vkey.to_str().unwrap(),
    ]);
    let expected = hex::encode(dugite_primitives::hash::blake2b_256(&raw).as_bytes());
    assert_eq!(printed.trim(), expected);
    assert_eq!(printed.trim().len(), 64, "32-byte hash = 64 hex chars");
}

#[test]
fn node_opcert_issue_flow() {
    let dir = tempfile::tempdir().unwrap();
    let cold_vkey = dir.path().join("cold.vkey");
    let cold_skey = dir.path().join("cold.skey");
    let counter = dir.path().join("opcert.counter");
    let kes_vkey = dir.path().join("kes.vkey");
    let kes_skey = dir.path().join("kes.skey");
    let opcert = dir.path().join("node.opcert");

    run_ok(&[
        "node",
        "key-gen",
        "--cold-verification-key-file",
        cold_vkey.to_str().unwrap(),
        "--cold-signing-key-file",
        cold_skey.to_str().unwrap(),
        "--operational-certificate-counter-file",
        counter.to_str().unwrap(),
    ]);
    run_ok(&[
        "node",
        "key-gen-kes",
        "--verification-key-file",
        kes_vkey.to_str().unwrap(),
        "--signing-key-file",
        kes_skey.to_str().unwrap(),
    ]);
    run_ok(&[
        "node",
        "issue-op-cert",
        "--kes-verification-key-file",
        kes_vkey.to_str().unwrap(),
        "--cold-signing-key-file",
        cold_skey.to_str().unwrap(),
        "--operational-certificate-counter-file",
        counter.to_str().unwrap(),
        "--kes-period",
        "5",
        "--out-file",
        opcert.to_str().unwrap(),
    ]);

    // Certificate envelope.
    let cert_env = read_envelope(&opcert);
    assert_eq!(cert_env["type"], "NodeOperationalCertificate");

    // Body: array(2)[array(4)[kes_vkey, seq=0, period=5, sig64], cold_vkey].
    let cert_cbor = hex::decode(cert_env["cborHex"].as_str().unwrap()).unwrap();
    let mut d = minicbor::Decoder::new(&cert_cbor);
    assert_eq!(d.array().unwrap(), Some(2));
    assert_eq!(d.array().unwrap(), Some(4));
    let hot_vkey = d.bytes().unwrap().to_vec();
    assert_eq!(
        hot_vkey,
        raw32_from_envelope(&read_envelope(&kes_vkey)),
        "hot vkey must be the generated KES vkey"
    );
    assert_eq!(d.u64().unwrap(), 0, "first opcert uses counter 0");
    assert_eq!(d.u64().unwrap(), 5, "kes period as passed");
    let sig = d.bytes().unwrap().to_vec();
    assert_eq!(sig.len(), 64);
    let cold_vk_bytes = d.bytes().unwrap().to_vec();
    assert_eq!(
        cold_vk_bytes,
        raw32_from_envelope(&read_envelope(&cold_vkey)),
        "embedded cold vkey must match the generated one"
    );

    // Signature verifies over the canonical OCertSignable layout.
    let signable = dugite_crypto::ocert::ocert_signable_bytes(&hot_vkey, 0, 5);
    dugite_crypto::keys::PaymentVerificationKey::from_bytes(&cold_vk_bytes)
        .unwrap()
        .verify(&signable, &sig)
        .expect("opcert signature must verify with the cold key");

    // Counter file advanced to 1.
    let counter_env = read_envelope(&counter);
    assert_eq!(
        counter_env["description"], "Next certificate issue number: 1",
        "counter must advance after issuing"
    );
    let counter_cbor = hex::decode(counter_env["cborHex"].as_str().unwrap()).unwrap();
    let mut d = minicbor::Decoder::new(&counter_cbor);
    assert_eq!(d.array().unwrap(), Some(2));
    assert_eq!(d.u64().unwrap(), 1);
}

#[test]
fn node_new_counter_writes_value_and_rejects_bad_vkey() {
    let dir = tempfile::tempdir().unwrap();
    let cold_vkey = dir.path().join("cold.vkey");
    let cold_skey = dir.path().join("cold.skey");
    let counter = dir.path().join("fresh.counter");

    run_ok(&[
        "node",
        "key-gen",
        "--cold-verification-key-file",
        cold_vkey.to_str().unwrap(),
        "--cold-signing-key-file",
        cold_skey.to_str().unwrap(),
        "--operational-certificate-counter-file",
        dir.path().join("orig.counter").to_str().unwrap(),
    ]);

    run_ok(&[
        "node",
        "new-counter",
        "--cold-verification-key-file",
        cold_vkey.to_str().unwrap(),
        "--counter-value",
        "42",
        "--operational-certificate-counter-file",
        counter.to_str().unwrap(),
    ]);

    let env = read_envelope(&counter);
    assert_eq!(env["type"], "NodeOperationalCertificateIssueCounter");
    assert_eq!(env["description"], "Next certificate issue number: 42");
    let cbor = hex::decode(env["cborHex"].as_str().unwrap()).unwrap();
    let mut d = minicbor::Decoder::new(&cbor);
    assert_eq!(d.array().unwrap(), Some(2));
    assert_eq!(d.u64().unwrap(), 42);

    // A vkey file without cborHex must be rejected.
    let bad_vkey = dir.path().join("bad.vkey");
    std::fs::write(&bad_vkey, r#"{"type": "StakePoolVerificationKey_ed25519"}"#).unwrap();
    assert_fails(&[
        "node",
        "new-counter",
        "--cold-verification-key-file",
        bad_vkey.to_str().unwrap(),
        "--counter-value",
        "1",
        "--operational-certificate-counter-file",
        dir.path().join("never.counter").to_str().unwrap(),
    ]);
}
