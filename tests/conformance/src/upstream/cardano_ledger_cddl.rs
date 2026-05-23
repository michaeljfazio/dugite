//! Phase 2 upstream conformance: validates CBOR against cardano-ledger CDDL schemas.
#![cfg(feature = "upstream-conformance")]
//!
//! **cddl crate adequacy assessment (0.9 and 0.10.5 both tested)**:
//!
//! The `conway.cddl` schema uses two advanced CDDL syntaxes that neither
//! version can parse:
//!
//! 1. **Range map keys** (`* 3 .. 255 => [* int64]` in `cost_models`):
//!    future Plutus language identifiers.
//!
//! 2. **Array map keys** (`{ + [tag, index] => [data, ex_units] }` in
//!    `redeemers` map form): the Conway era redeemer encoding.
//!
//! Even if the schema parsed, CDDL group expansion in arrays (used by
//! `header_body`) is handled incorrectly — the crate counts group references
//! as single entries but CBOR inlines them.
//!
//! **Decision**: use simplified structural CDDL strings for Phase 2.  These
//! strings capture the top-level invariants (field count, CBOR major type) that
//! matter for catching encoding bugs.  A follow-up issue tracks migration to a
//! complete Rust CDDL validator once one is available.
//!
//! **Covered**: Conway `transaction` structural check (4-element array with
//! correct CBOR types at each position).

use std::path::Path;

use super::ouroboros_consensus::unwrap_tag24;

// ── CBOR helpers ──────────────────────────────────────────────────────────────

/// Extract the CBOR bytes of the item at `index` within the outermost array.
fn cbor_array_item(data: &[u8], index: usize) -> Vec<u8> {
    let mut dec = minicbor::Decoder::new(data);
    dec.array()
        .unwrap_or_else(|e| panic!("cbor_array_item: expected array: {e}"));
    for _ in 0..index {
        dec.skip()
            .unwrap_or_else(|e| panic!("cbor_array_item: skip[{index}] failed: {e}"));
    }
    let start = dec.position();
    dec.skip()
        .unwrap_or_else(|e| panic!("cbor_array_item: skip item failed: {e}"));
    data[start..dec.position()].to_vec()
}

// ── Validation ────────────────────────────────────────────────────────────────

fn validate(cddl_text: &str, rule: &str, cbor: &[u8], label: &str) {
    let entry = if cddl_text.trim_start().starts_with(&format!("{rule} =")) {
        cddl_text.to_owned()
    } else {
        format!("_entry = {rule}\n{cddl_text}")
    };
    if let Err(e) = cddl::validate_cbor_from_slice(&entry, cbor, None) {
        panic!("{label}: CDDL validation failed:\n{e:#?}");
    }
}

// ── Structural CDDL schemas ───────────────────────────────────────────────────
//
// Simplified but faithful: constrain each position to its correct CBOR major
// type (map / bool / null) without recursing into the full Cardano schema.

/// Conway `transaction = [transaction_body, transaction_witness_set, bool,
///                        auxiliary_data / nil]`
const CONWAY_TRANSACTION_CDDL: &str = r#"
transaction = [transaction_body, transaction_witness_set, bool, (auxiliary_data / null)]
transaction_body        = {+ any => any}
transaction_witness_set = {* any => any}
auxiliary_data          = {+ any => any}
"#;

// ── Run all checks ────────────────────────────────────────────────────────────

/// Run all Phase 2 CDDL checks.  Called from `upstream_tests.rs`.
pub fn run_all_checks(ledger_dir: &Path, ouroboros_dir: &Path) {
    let _ = ledger_dir;

    // ── Conway transaction ────────────────────────────────────────────────
    // GenTx network format: [era_tag, tag24(tx_cbor)]
    let conway_gentx = ouroboros_dir.join("cardano/CardanoNodeToNodeVersion2/GenTx_Conway");
    if conway_gentx.exists() {
        let raw = std::fs::read(&conway_gentx).expect("read GenTx_Conway");
        let tag24_tx = cbor_array_item(&raw, 1);
        let tx_cbor = unwrap_tag24(&tag24_tx);
        validate(
            CONWAY_TRANSACTION_CDDL,
            "transaction",
            &tx_cbor,
            "conway_transaction",
        );
        eprintln!("[cddl] conway_transaction: structural check [map,map,bool,nil|map] ✓");
    } else {
        eprintln!("[cddl] SKIP conway_transaction: GenTx_Conway not in fixture");
    }

    // ── Block, header, governance action ─────────────────────────────────
    // cddl 0.10.5 cannot parse conway.cddl (range map keys in cost_models;
    // array map keys in redeemers) and mishandles CDDL group expansion in
    // header_body.  These checks are deferred.
    eprintln!(
        "[cddl] NOTE: conway_block / conway_header / conway_govaction deferred — \
         cddl 0.10.5 cannot parse `cost_models` (range map key) or `redeemers` \
         (array map key) in conway.cddl, and does not expand CDDL groups in arrays."
    );

    eprintln!("[cddl] Phase 2 CDDL checks complete");
}
