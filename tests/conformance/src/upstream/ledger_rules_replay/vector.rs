//! CBOR decode for ImpSpec dump vectors.
//!
//! The dugite-fixture-gen Haskell generator produces **5 separate CBOR files
//! per test case directory** (the original 4 ImpSpec files plus the expected
//! final state):
//!
//! ```text
//! conformance_dump_ctx.cbor     — ExecContext  (CBOR null `F6` for NEWEPOCH — EncCBOR () = encodeNull)
//! conformance_dump_env.cbor     — Environment  (CBOR null `F6` for NEWEPOCH — EncCBOR () = encodeNull)
//! conformance_dump_st.cbor      — State = NewEpochState as array(7)  (initial, before transition)
//! conformance_dump_sig.cbor     — Signal (u64 EpochNo for NEWEPOCH; tx CBOR for UTXO)
//! conformance_dump_st_out.cbor  — Expected final state from Haskell (optional — absent when STS rejects)
//! ```
//!
//! The first 4 files are required (`REQUIRED_FILES`).  `st_out` is optional:
//! real Haskell vectors have it; synthetic fixtures and rejected-transition
//! vectors do not.
//!
//! The `rule` is derived from the parent directory name (e.g. "ConwayNEWEPOCH",
//! "ConwayUTXO").
//!
//! ## NewEpochState array(7) field layout
//!
//! ```text
//! [0] EpochNo             u64
//! [1] BlocksMade(prev)    map (empty = a0)
//! [2] BlocksMade(cur)     map (empty = a0)
//! [3] EpochState          array(4) — AccountState + LedgerState + Snapshots + NonMyopic
//! [4] StrictMaybe         array(0)=Nothing or array(1)=Just
//! [5] PoolDistr           map
//! [6] stashedAVVM         array(0) in Conway
//! ```

use std::path::Path;

/// A test vector decoded from a test-case ImpSpec dump directory.
///
/// All CBOR blobs are kept as raw bytes so that downstream modules
/// (`bridge.rs`, `runner.rs`, `compare.rs`) can inspect them without
/// coupling to a particular typed decode path.
#[derive(Debug)]
pub struct ImpVector {
    /// Ledger rule name derived from the directory name (e.g. "ConwayNEWEPOCH").
    pub rule: String,
    /// Raw CBOR bytes of `conformance_dump_ctx.cbor` (ExecContext).
    pub ctx_cbor: Vec<u8>,
    /// Raw CBOR bytes of `conformance_dump_env.cbor` (Environment).
    pub env_cbor: Vec<u8>,
    /// Raw CBOR bytes of `conformance_dump_st.cbor` (State = NewEpochState array(7), initial).
    pub st_cbor: Vec<u8>,
    /// Raw CBOR bytes of `conformance_dump_sig.cbor` (Signal).
    pub sig_cbor: Vec<u8>,
    /// Raw CBOR bytes of `conformance_dump_st_out.cbor` (expected final state from Haskell).
    ///
    /// `None` when the file is absent — e.g. for synthetic fixtures or when the
    /// Haskell STS rule rejected the transition (signal out of range, idempotent epoch, etc.).
    /// Real Haskell-generated vectors always include this file for successful transitions.
    pub st_out_cbor: Option<Vec<u8>>,
}

/// Decode an ImpSpec dump vector from a test-case directory.
///
/// `dir` must contain the 4 required files:
///   - `conformance_dump_ctx.cbor`
///   - `conformance_dump_env.cbor`
///   - `conformance_dump_st.cbor`
///   - `conformance_dump_sig.cbor`
///
/// The optional 5th file `conformance_dump_st_out.cbor` (Haskell expected
/// final state) is read if present; `st_out_cbor` is `None` otherwise.
///
/// The `rule` field is taken from `dir`'s parent directory name
/// (the rule directory, e.g. `ConwayNEWEPOCH/test_minimal_epoch_advance` →
/// `rule = "ConwayNEWEPOCH"`).
///
/// Returns `Err` with a human-readable message on any missing required file
/// or I/O failure.
pub fn decode_vector(dir: &Path) -> Result<ImpVector, String> {
    let rule = rule_from_dir(dir);

    let ctx_cbor = read_file(dir, "conformance_dump_ctx.cbor")?;
    let env_cbor = read_file(dir, "conformance_dump_env.cbor")?;
    let st_cbor = read_file(dir, "conformance_dump_st.cbor")?;
    let sig_cbor = read_file(dir, "conformance_dump_sig.cbor")?;

    // Optional 5th file: Haskell's expected final state.
    // Present in real Haskell-generated vectors; absent in synthetic fixtures.
    let st_out_cbor = std::fs::read(dir.join("conformance_dump_st_out.cbor")).ok();

    Ok(ImpVector {
        rule,
        ctx_cbor,
        env_cbor,
        st_cbor,
        sig_cbor,
        st_out_cbor,
    })
}

/// Derive the rule name from a test-case directory.
///
/// The convention is:
///   `<fixtures>/<rule>/<test_name>/`
///
/// so we walk up to the parent and take its name.
/// Falls back to the directory's own name if there is no parent.
fn rule_from_dir(dir: &Path) -> String {
    // Try parent (rule dir) → grandparent would be the fixture root.
    if let Some(parent) = dir.parent() {
        if let Some(name) = parent.file_name() {
            let s = name.to_string_lossy().into_owned();
            // If the parent name looks like a rule (starts with uppercase and contains
            // only alphanumeric/_), use it; otherwise fall through to the dir name.
            if s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                return s;
            }
        }
    }
    // Fallback: use the directory name itself.
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

fn read_file(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let path = dir.join(name);
    std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))
}
