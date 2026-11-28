//! Block-path decode conformance against the r2rationality `cardano-cbor-dataset`
//! (the corpus pinned in `tests/conformance/node-diversity/corpora.toml`).
//!
//! The dataset classifies every sample by what ITS pinned Haskell decoder does with it:
//! `valid` / `manual-valid` decode, `invalid-zap-*` / `manual-invalid` are refused.  The
//! dataset Dockerfile pins cardano-ledger `6176e413`, which is NEWER than the `f649f975` of
//! cardano-node 11.1.3, so the classification is not the node's in two known places:
//!
//! * `header` and `header_body`: the dataset marks 34 and 23 indefinite samples valid, but
//!   they are invalid for 11.1.3, where the Praos header is `RecD` (see #1224);
//! * `auxiliary_data_array`: the dataset classifies it with the non-annotator decoder, not
//!   the transaction-path one (see the #1169 comment).
//!
//! A future corpus bump must be checked against these deltas and must not push the decoders
//! the wrong way (these tests only drive `block`, `transaction`, `babbage_transaction_output`,
//! `data` and `network_id`; the header is covered by `recd_definite_only.rs`).  This
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
/// * A sample Haskell REJECTS must not decode, except for the samples NAMED in
///   `known_over_acceptance`: pre-existing over-acceptances that are not part of the
///   indefinite-length / embedded-CBOR / network-id fixes (block-segment consistency that
///   dugite does not check: witness-set count against body count, auxiliary-data key range or
///   repeat, `invalid_transactions` range / order / repeat, and a few value / required-key
///   strictness cases). The list is pinned by NAME, not by count, so a sample can neither
///   be swapped for another nor silently grow the set: the test fails when the observed set
///   differs from the list in either direction.
///
/// The corpus comes from `just ndc-fetch` (`scripts/node-diversity/fetch-corpora.sh cbor`).
/// When it is missing the test is skipped, unless `DUGITE_CBOR_DATASET_REQUIRE=1` (set by the CI job
/// that fetches it, as for the `ndc_reports` tests), where a missing corpus FAILS.
fn run(rule: &str, known_over_acceptance: &[&str], decode: &dyn Fn(&[u8]) -> Result<(), String>) {
    let Some(root) = dataset_root() else {
        assert!(
            std::env::var("DUGITE_CBOR_DATASET_REQUIRE").as_deref() != Ok("1"),
            "{rule}: DUGITE_CBOR_DATASET_REQUIRE=1 but the cbor-dataset corpus is missing; \
             run `scripts/node-diversity/fetch-corpora.sh cbor`"
        );
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
    let mut observed: Vec<String> = over
        .iter()
        .map(|l| {
            l.strip_prefix(&format!("{rule}/"))
                .unwrap_or(l)
                .split(": ")
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    observed.sort();
    let mut expected: Vec<String> = known_over_acceptance
        .iter()
        .map(|s| s.to_string())
        .collect();
    expected.sort();
    assert_eq!(
        observed, expected,
        "rule `{rule}`: the samples Haskell rejects that still decode differ from the pinned list"
    );
}

/// Block samples Haskell rejects that `decode_block` still accepts (block-segment consistency).
const BLOCK_OVER_ACCEPTED: &[&str] = &[
    "invalid-zap-0/00462-d77730577a5b3986.input.cbor",
    "invalid-zap-0/00547-070cfd6e9914672a.input.cbor",
    "invalid-zap-1/00061-de40d7fcb83a7099.input.cbor",
    "invalid-zap-3/00515-7b5fc28dda362af9.input.cbor",
    "invalid-zap-3/00581-db003ce8b95596e3.input.cbor",
    "manual-invalid/auxiliary-data-key-past-transaction-index.input.cbor",
    "manual-invalid/auxiliary-data-key-twice.input.cbor",
    "manual-invalid/invalid-transactions-repeated.input.cbor",
    "manual-invalid/invalid-transactions-unsorted.input.cbor",
];

/// Additional samples only `decode_block_minimal` accepts: it skips the witness sets.
const BLOCK_MINIMAL_ONLY: &[&str] = &[
    "invalid-zap-0/00035-80e995ebe3d97a54.input.cbor",
    "invalid-zap-1/00438-0cff8e86c00c0724.input.cbor",
    "invalid-zap-1/00479-63e09d3ea2c6f651.input.cbor",
    "invalid-zap-2/00110-fd03bb2f9d4ea0fe.input.cbor",
];

fn minimal_over_accepted() -> Vec<&'static str> {
    BLOCK_OVER_ACCEPTED
        .iter()
        .chain(BLOCK_MINIMAL_ONLY)
        .copied()
        .collect()
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
    run("block", BLOCK_OVER_ACCEPTED, &|b| {
        decode_block(&hfc_wrap_conway(b))
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
}

/// Minimal mode skips the witness sets, so it cannot see the witness-set-level zaps.
#[test]
fn dataset_block_decode_block_minimal() {
    run("block", &minimal_over_accepted(), &|b| {
        decode_block_minimal(&hfc_wrap_conway(b))
            .map(|_| ())
            .map_err(|e| e.to_string())
    });
}

/// Every block Haskell accepts must also be measurable and hashable: the sync pipeline
/// extracts the body segments (`validate_block_body_hash`) and the body size (BBODY) from the
/// raw bytes after decoding, and refuses a block it cannot walk. Indefinite (`9f`) and wide
/// (`9b`) block-array heads are in the dataset's `manual-valid` samples.
#[test]
fn dataset_valid_blocks_pass_body_extraction() {
    let Some(root) = dataset_root() else {
        assert!(
            std::env::var("DUGITE_CBOR_DATASET_REQUIRE").as_deref() != Ok("1"),
            "DUGITE_CBOR_DATASET_REQUIRE=1 but the cbor-dataset corpus is missing"
        );
        eprintln!("SKIP: cbor-dataset not fetched (run `just ndc-fetch`)");
        return;
    };
    let rule_dir = root.join("block");
    let mut checked = 0;
    for class in ACCEPT {
        for (name, bytes) in samples(&rule_dir, class) {
            let blk = hfc_wrap_conway(&bytes);
            let comps = dugite_serialization::extract_block_body_components(&blk)
                .unwrap_or_else(|| panic!("{class}/{name}: no body components"));
            assert!(
                comps.len() >= 3,
                "{class}/{name}: {} components",
                comps.len()
            );
            assert!(
                dugite_serialization::compute_block_body_size_from_cbor(&blk).is_some(),
                "{class}/{name}: body size"
            );
            let body = dugite_serialization::extract_block_body_cbor(&blk)
                .unwrap_or_else(|| panic!("{class}/{name}: no body bytes"));
            let joined: usize = comps.iter().map(|c| c.len()).sum();
            assert_eq!(joined, body.len(), "{class}/{name}");
            checked += 1;
        }
    }
    assert!(
        checked >= 19,
        "only {checked} valid block samples were found"
    );
}

#[test]
fn dataset_transaction_decode_transaction() {
    run(
        "transaction",
        &["manual-invalid/auto-array-3-a-value-matching-none-of-60.input.cbor"],
        &|b| {
            decode_transaction(6, b)
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
    );
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
        run("data", &[], &|b| {
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
    run("network_id", &[], &|b| {
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
    run(
        "babbage_transaction_output",
        &["manual-invalid/arity-few-1-entries.input.cbor"],
        &|b| {
            decode_transaction_output(6, b)
                .map(|_| ())
                .map_err(|e| e.to_string())
        },
    );
}
