//! Block-path decode conformance against the r2rationality `cardano-cbor-dataset`
//! (the corpus pinned in `tests/conformance/node-diversity/corpora.toml`).
//!
//! The dataset classifies every sample by what the Haskell node decoder does with it:
//! `valid` / `manual-valid` decode, `invalid-zap-*` / `manual-invalid` are refused.  This
//! test drives the samples through the REAL dugite entry points the node uses
//! (`decode_block`, `decode_block_minimal`, `decode_transaction`, `decode_transaction_output`)
//! and requires the same verdict.
//!
//! The corpus is fetched by `just ndc-fetch` into `target/node-diversity/corpora`.  When it is
//! absent the test is skipped (and says so), so a plain `cargo test` stays hermetic.
//! `DUGITE_DATASET_SHOW=500` prints more than the first 15 mismatches.

use dugite_serialization::{
    decode_block, decode_block_minimal, decode_transaction, decode_transaction_output,
};
use std::path::{Path, PathBuf};

fn dataset_root() -> Option<PathBuf> {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.push("target/node-diversity/corpora/cbor-dataset/dataset/conway");
    p.is_dir().then_some(p)
}

fn samples(rule_dir: &Path, class: &str) -> Vec<(String, Vec<u8>)> {
    let dir = rule_dir.join(class);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut v: Vec<_> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(".input.cbor"))
        })
        .map(|p| {
            (
                format!("{}/{}", class, p.file_name().unwrap().to_string_lossy()),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

const ACCEPT: &[&str] = &["valid", "manual-valid"];
const REJECT: &[&str] = &[
    "invalid-zap-0",
    "invalid-zap-1",
    "invalid-zap-2",
    "invalid-zap-3",
    "manual-invalid",
];

/// `(under, over)`: samples Haskell accepts that dugite rejects, and samples Haskell rejects
/// that dugite accepts.
fn mismatches(
    root: &Path,
    rule: &str,
    decode: &dyn Fn(&[u8]) -> Result<(), String>,
) -> (Vec<String>, Vec<String>) {
    let rule_dir = root.join(rule);
    let (mut under, mut over) = (Vec::new(), Vec::new());
    for class in ACCEPT {
        for (name, bytes) in samples(&rule_dir, class) {
            if let Err(e) = decode(&bytes) {
                under.push(format!(
                    "{rule}/{name}: Haskell accepts, dugite rejects: {e}"
                ));
            }
        }
    }
    for class in REJECT {
        for (name, bytes) in samples(&rule_dir, class) {
            if decode(&bytes).is_ok() {
                over.push(format!("{rule}/{name}: Haskell rejects, dugite accepts"));
            }
        }
    }
    (under, over)
}

/// Drive every sample of `rule` through `decode`.
///
/// * A sample Haskell ACCEPTS must decode, always: a refusal here is the stall defect (a
///   valid on-chain block that no peer can deliver).
/// * A sample Haskell REJECTS must not decode, except for `known_over_acceptance` samples:
///   pre-existing over-acceptances that are NOT part of the indefinite-length / embedded-CBOR
///   / network-id fixes.  In `block` every one of them is a block-segment consistency check
///   dugite does not make (witness-set count against body count, auxiliary-data key range or
///   repeat, `invalid_transactions` range / order / repeat); the single `transaction` sample
///   and the three `babbage_transaction_output` samples are value / required-key strictness.
///   The figure is a ratchet: the test fails when it is not exact, so fixing one of them
///   forces the number down and it can never silently grow.
fn run(rule: &str, known_over_acceptance: usize, decode: &dyn Fn(&[u8]) -> Result<(), String>) {
    let Some(root) = dataset_root() else {
        eprintln!("SKIP {rule}: cbor-dataset not fetched (run `just ndc-fetch`)");
        return;
    };
    let (under, over) = mismatches(&root, rule, decode);
    let show: usize = std::env::var("DUGITE_DATASET_SHOW")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    assert!(
        under.is_empty(),
        "{} samples Haskell accepts fail to decode for rule `{rule}` (first {show}):\n{}",
        under.len(),
        under
            .iter()
            .take(show)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(
        over.len(),
        known_over_acceptance,
        "rule `{rule}`: {} samples Haskell rejects still decode, expected exactly \
         {known_over_acceptance} (lower the figure if you fixed one) (first {show}):\n{}",
        over.len(),
        over.iter()
            .take(show)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// The dataset's `block` rule is the bare Conway era block; the node receives it inside the
/// HFC envelope `[era_index, block]` (Conway = 7: Byron takes two indices, EBB and main).
fn hfc_wrap_conway(block: &[u8]) -> Vec<u8> {
    let mut v = vec![0x82, 0x07];
    v.extend_from_slice(block);
    v
}

#[test]
fn dataset_block_decode_block() {
    run("block", 31, &|b| {
        decode_block(&hfc_wrap_conway(b))
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
}

/// Minimal mode skips the witness sets, so it cannot see the witness-set-level zaps.
#[test]
fn dataset_block_decode_block_minimal() {
    run("block", 39, &|b| {
        decode_block_minimal(&hfc_wrap_conway(b))
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
}

#[test]
fn dataset_transaction_decode_transaction() {
    run("transaction", 1, &|b| {
        decode_transaction(6, b)
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
}

fn addr_bytes_item() -> Vec<u8> {
    let mut a = vec![0x58, 29, 0x61];
    a.extend([0x11u8; 28]);
    a
}

/// The `data` rule is `#6.24(bytes .cbor plutus_data)`: embed it as the inline datum of a
/// map-form output `{0: addr, 1: 0, 2: [1, <data>]}`, the position the ledger decodes it from.
#[test]
fn dataset_data_as_inline_datum() {
    for era in [5u16, 6] {
        run("data", 0, &|b| {
            let mut out = vec![0xa3, 0x00];
            out.extend(addr_bytes_item());
            out.extend([0x01, 0x00, 0x02, 0x82, 0x01]);
            out.extend_from_slice(b);
            decode_transaction_output(era, &out)
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }
}

/// The `network_id` rule is body key 15: `{0: [], 1: [], 2: 0, 15: <sample>}` inside a tx.
#[test]
fn dataset_network_id_in_tx_body() {
    run("network_id", 0, &|b| {
        let mut tx = vec![0x84, 0xa4, 0x00, 0x80, 0x01, 0x80, 0x02, 0x00, 0x0f];
        tx.extend_from_slice(b);
        tx.extend([0xa0, 0xf5, 0xf6]);
        decode_transaction(6, &tx)
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
}

#[test]
fn dataset_babbage_transaction_output() {
    run("babbage_transaction_output", 3, &|b| {
        decode_transaction_output(6, b)
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
}
