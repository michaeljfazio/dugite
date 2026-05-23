//! Phase 4 — Bridge: inspect and decode ImpSpec CBOR blobs.
//!
//! Each ImpSpec test case is stored as 4 separate CBOR files:
//!   conformance_dump_ctx.cbor  — ExecContext
//!   conformance_dump_env.cbor  — Environment
//!   conformance_dump_st.cbor   — State (NewEpochState array(7))
//!   conformance_dump_sig.cbor  — Signal (u64 EpochNo for NEWEPOCH, tx CBOR for UTXO)
//!
//! This module provides:
//!
//! 1. `DecodedState` — a lightweight wrapper that carries the raw bytes together
//!    with a human-readable shape description extracted via minicbor.
//!
//! 2. `decode_state(cbor, label)` — entry point for tests; returns a
//!    `DecodedState` or an error string.
//!
//! 3. `decode_epoch_no(sig_cbor)` — reads a CBOR u64 from the signal file
//!    (used for NEWEPOCH rule where the signal is the target epoch number).
//!
//! 4. `decode_initial_epoch_no(st_cbor)` — reads field [0] of the array(7)
//!    NewEpochState, which is the current epoch number before the transition.
//!
//! 5. `decode_new_epoch_state(st_cbor)` — structurally decodes the full
//!    `array(7)` NewEpochState, extracting: EpochNo, BlocksMade counts,
//!    AccountState (treasury + reserves), StrictMaybe shape, PoolDistr entry
//!    count, and stashedAVVM shape. LedgerState, Snapshots, and NonMyopic
//!    sub-fields are recorded as raw CBOR byte ranges (stub — Phase 4 follow-on).
//!
//! ## NewEpochState array(7) field layout (Haskell / cardano-ledger)
//!
//! ```text
//! [0] nesEL      :: EpochNo              u64
//! [1] nesBprev   :: BlocksMade           map pool_keyhash → u64
//! [2] nesBcur    :: BlocksMade           map pool_keyhash → u64
//! [3] nesEs       :: EpochState          array(4)
//!     [3.0] AccountState  :: [treasury: Coin, reserves: Coin]   array(2) u64s
//!     [3.1] LedgerState   :: <sub-tree, skipped — stub>
//!     [3.2] EpochSnapshots:: <sub-tree, skipped — stub>
//!     [3.3] NonMyopic     :: <sub-tree, skipped — stub>
//! [4] nesRu       :: StrictMaybe PulsingRewUpdate  array(0)=Nothing | array(1)=Just
//! [5] nesPd       :: PoolDistr           map (with rational total stake)
//! [6] stashedAVVM :: ()                  array(0) in Conway (always empty)
//! ```
//!
//! ## Future work (Phase 4 follow-on)
//!
//! Once `dugite-ledger` exposes a public deserialization API for
//! `NewEpochState` / `LedgerState`, replace the raw-CBOR path with a full
//! typed decode so that `runner.rs` can call `Ledger::apply_tx` directly.

use minicbor::data::Type;

// ── DecodedNewEpochState ──────────────────────────────────────────────────────

/// Structural decode of a NewEpochState `array(7)` blob.
///
/// Fields that require deep sub-tree mapping (LedgerState, Snapshots,
/// NonMyopic) are recorded only as their raw byte length.  All other fields
/// are fully decoded.
///
/// This struct is returned by [`decode_new_epoch_state`].
#[derive(Debug)]
pub struct DecodedNewEpochState {
    /// field[0] — current epoch number before the transition.
    pub epoch_no: u64,
    /// field[1] — BlocksMade(prev): number of entries in the map.
    pub blocks_prev_count: u64,
    /// field[2] — BlocksMade(cur): number of entries in the map.
    pub blocks_cur_count: u64,
    /// field[3.0] — AccountState treasury (lovelace).
    pub treasury: u64,
    /// field[3.0] — AccountState reserves (lovelace).
    pub reserves: u64,
    /// field[3.1] — LedgerState: raw CBOR byte length (sub-tree skipped).
    pub ledger_state_cbor_len: usize,
    /// field[3.2] — EpochSnapshots: raw CBOR byte length (sub-tree skipped).
    pub snapshots_cbor_len: usize,
    /// field[3.3] — NonMyopic: raw CBOR byte length (sub-tree skipped).
    pub nonmyopic_cbor_len: usize,
    /// field[4] — StrictMaybe shape: `None` for Nothing (array(0)), `Some(n)` for Just (array(1)).
    pub pulsing_rew_update: StrictMaybe,
    /// field[5] — PoolDistr: number of entries in the map.
    pub pool_distr_count: u64,
    /// field[6] — stashedAVVM shape: must be array(0) in Conway.
    pub stashed_avvm_len: Option<u64>,
}

/// A decoded `StrictMaybe` (Haskell `SJust` / `SNothing`).
#[derive(Debug, PartialEq, Eq)]
pub enum StrictMaybe {
    /// `SNothing` — encoded as `array(0)` (CBOR `0x80`).
    Nothing,
    /// `SJust` — encoded as `array(1)`.
    Just,
}

/// Decode the structural fields of a NewEpochState `array(7)` blob.
///
/// Returns a [`DecodedNewEpochState`] on success, or an error string describing
/// the first decode failure.
///
/// The three large sub-trees (LedgerState, Snapshots, NonMyopic) are skipped
/// via `minicbor::Decoder::skip()` and recorded only as byte lengths.
pub fn decode_new_epoch_state(st_cbor: &[u8]) -> Result<DecodedNewEpochState, String> {
    if st_cbor.is_empty() {
        return Err("st_cbor is empty".to_string());
    }
    let mut dec = minicbor::Decoder::new(st_cbor);

    // ── Outer array(7) ────────────────────────────────────────────────────────
    match dec
        .array()
        .map_err(|e| format!("NewEpochState outer: {e}"))?
    {
        Some(7) => {}
        Some(n) => return Err(format!("NewEpochState: expected array(7), got array({n})")),
        None => return Err("NewEpochState: expected definite array(7), got indefinite".to_string()),
    }

    // ── field[0] EpochNo (u64) ────────────────────────────────────────────────
    let epoch_no = decode_u64(&mut dec, "field[0] EpochNo")?;

    // ── field[1] BlocksMade(prev) map ─────────────────────────────────────────
    let blocks_prev_count = decode_map_count(&mut dec, "field[1] BlocksMade(prev)")?;

    // ── field[2] BlocksMade(cur) map ──────────────────────────────────────────
    let blocks_cur_count = decode_map_count(&mut dec, "field[2] BlocksMade(cur)")?;

    // ── field[3] EpochState array(4) ─────────────────────────────────────────
    match dec
        .array()
        .map_err(|e| format!("field[3] EpochState: {e}"))?
    {
        Some(4) => {}
        Some(n) => {
            return Err(format!(
                "field[3] EpochState: expected array(4), got array({n})"
            ))
        }
        None => {
            return Err(
                "field[3] EpochState: expected definite array(4), got indefinite".to_string(),
            )
        }
    }

    // ── field[3.0] AccountState array(2): [treasury, reserves] ───────────────
    match dec
        .array()
        .map_err(|e| format!("field[3.0] AccountState: {e}"))?
    {
        Some(2) => {}
        Some(n) => {
            return Err(format!(
                "field[3.0] AccountState: expected array(2), got array({n})"
            ))
        }
        None => {
            return Err(
                "field[3.0] AccountState: expected definite array(2), got indefinite".to_string(),
            )
        }
    }
    let treasury = decode_u64(&mut dec, "field[3.0] treasury")?;
    let reserves = decode_u64(&mut dec, "field[3.0] reserves")?;

    // ── field[3.1] LedgerState (skip — stub) ─────────────────────────────────
    let before_ls = dec.position();
    dec.skip()
        .map_err(|e| format!("field[3.1] LedgerState skip: {e}"))?;
    let ledger_state_cbor_len = dec.position() - before_ls;

    // ── field[3.2] EpochSnapshots (skip — stub) ───────────────────────────────
    let before_snap = dec.position();
    dec.skip()
        .map_err(|e| format!("field[3.2] Snapshots skip: {e}"))?;
    let snapshots_cbor_len = dec.position() - before_snap;

    // ── field[3.3] NonMyopic (skip — stub) ────────────────────────────────────
    let before_nm = dec.position();
    dec.skip()
        .map_err(|e| format!("field[3.3] NonMyopic skip: {e}"))?;
    let nonmyopic_cbor_len = dec.position() - before_nm;

    // ── field[4] StrictMaybe PulsingRewUpdate ─────────────────────────────────
    let pulsing_rew_update = decode_strict_maybe(&mut dec, "field[4] StrictMaybe")?;

    // ── field[5] PoolDistr map ─────────────────────────────────────────────────
    let pool_distr_count = decode_map_count(&mut dec, "field[5] PoolDistr")?;

    // ── field[6] stashedAVVM — array(0) in Conway ─────────────────────────────
    let stashed_avvm_len = decode_stashed_avvm(&mut dec, "field[6] stashedAVVM")?;

    Ok(DecodedNewEpochState {
        epoch_no,
        blocks_prev_count,
        blocks_cur_count,
        treasury,
        reserves,
        ledger_state_cbor_len,
        snapshots_cbor_len,
        nonmyopic_cbor_len,
        pulsing_rew_update,
        pool_distr_count,
        stashed_avvm_len,
    })
}

// ── Internal decode helpers ───────────────────────────────────────────────────

/// Decode a CBOR unsigned integer as `u64`.
fn decode_u64(dec: &mut minicbor::Decoder<'_>, label: &str) -> Result<u64, String> {
    match dec
        .datatype()
        .map_err(|e| format!("{label} datatype: {e}"))?
    {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            dec.u64().map_err(|e| format!("{label}: {e}"))
        }
        other => Err(format!("{label}: expected unsigned integer, got {other:?}")),
    }
}

/// Decode a definite-length CBOR map and return its entry count.
///
/// For indefinite maps, consumes all key-value pairs and returns the count.
/// Does not decode the map entries — just counts them (for BlocksMade and
/// PoolDistr which may have arbitrary pool key hashes as keys).
fn decode_map_count(dec: &mut minicbor::Decoder<'_>, label: &str) -> Result<u64, String> {
    match dec.map().map_err(|e| format!("{label} map header: {e}"))? {
        Some(n) => {
            // Definite map: skip all n key-value pairs.
            for i in 0..n {
                dec.skip()
                    .map_err(|e| format!("{label} map key[{i}]: {e}"))?;
                dec.skip()
                    .map_err(|e| format!("{label} map val[{i}]: {e}"))?;
            }
            Ok(n)
        }
        None => {
            // Indefinite map: walk until break.
            let mut count = 0u64;
            loop {
                if dec
                    .datatype()
                    .map_err(|e| format!("{label} indef map type: {e}"))?
                    == Type::Break
                {
                    dec.skip()
                        .map_err(|e| format!("{label} indef map break: {e}"))?;
                    break;
                }
                dec.skip()
                    .map_err(|e| format!("{label} indef map key[{count}]: {e}"))?;
                dec.skip()
                    .map_err(|e| format!("{label} indef map val[{count}]: {e}"))?;
                count += 1;
            }
            Ok(count)
        }
    }
}

/// Decode a `StrictMaybe` value.
///
/// Haskell's `StrictMaybe` (from `cardano-strict-containers`) is CBOR-encoded
/// as `array(0)` for `SNothing` and `array(1)` for `SJust v`.
/// When `SJust`, the inner value is skipped (we record the shape only).
fn decode_strict_maybe(
    dec: &mut minicbor::Decoder<'_>,
    label: &str,
) -> Result<StrictMaybe, String> {
    match dec
        .array()
        .map_err(|e| format!("{label} array header: {e}"))?
    {
        Some(0) => Ok(StrictMaybe::Nothing),
        Some(1) => {
            // SJust: skip the inner value.
            dec.skip()
                .map_err(|e| format!("{label} SJust inner: {e}"))?;
            Ok(StrictMaybe::Just)
        }
        Some(n) => Err(format!(
            "{label}: expected array(0) or array(1) for StrictMaybe, got array({n})"
        )),
        None => {
            // Indefinite array is not valid for StrictMaybe.
            Err(format!(
                "{label}: expected definite array for StrictMaybe, got indefinite"
            ))
        }
    }
}

/// Decode the `stashedAVVM` field.
///
/// In Conway this is always encoded as `array(0)` (unit / empty).
/// Returns `Some(n)` with the array length, or `Err` if the encoding is
/// not an array at all.  A non-zero length is reported as a warning in the
/// caller rather than an error — pre-Conway chains may have non-empty AVVM
/// entries in historical snapshots.
fn decode_stashed_avvm(
    dec: &mut minicbor::Decoder<'_>,
    label: &str,
) -> Result<Option<u64>, String> {
    match dec
        .datatype()
        .map_err(|e| format!("{label} datatype: {e}"))?
    {
        Type::Array => {
            let n = dec
                .array()
                .map_err(|e| format!("{label} array header: {e}"))?;
            if let Some(len) = n {
                // Definite: skip all elements.
                for i in 0..len {
                    dec.skip()
                        .map_err(|e| format!("{label} element[{i}]: {e}"))?;
                }
                Ok(Some(len))
            } else {
                // Indefinite array: consume until break.
                let mut count = 0u64;
                loop {
                    if dec
                        .datatype()
                        .map_err(|e| format!("{label} indef elem type: {e}"))?
                        == Type::Break
                    {
                        dec.skip()
                            .map_err(|e| format!("{label} indef break: {e}"))?;
                        break;
                    }
                    dec.skip()
                        .map_err(|e| format!("{label} indef elem[{count}]: {e}"))?;
                    count += 1;
                }
                Ok(Some(count))
            }
        }
        // Might be encoded as unit/null in some edge case.
        Type::Null | Type::Undefined => {
            dec.skip()
                .map_err(|e| format!("{label} null/undef skip: {e}"))?;
            Ok(None)
        }
        other => Err(format!("{label}: expected array, got {other:?}")),
    }
}

/// A partially-decoded ledger state blob.
///
/// `raw_cbor` is passed through unchanged to `compare.rs`; `shape` is shown
/// in diagnostic output when comparisons fail.
pub struct DecodedState {
    /// Raw CBOR bytes from the dump vector.
    pub raw_cbor: Vec<u8>,
    /// Human-readable top-level CBOR shape (e.g. `"arr[7]"`, `"arr[2]"`).
    pub shape: String,
}

/// Decode the top-level CBOR shape and return a `DecodedState`.
///
/// Returns `Err` with a human-readable message on any decode failure.
pub fn decode_state(cbor: &[u8], label: &str) -> Result<DecodedState, String> {
    let shape = top_level_shape(cbor, label)?;
    Ok(DecodedState {
        raw_cbor: cbor.to_vec(),
        shape,
    })
}

/// Decode the signal file for a NEWEPOCH rule as a CBOR u64.
///
/// The signal for NEWEPOCH is the target `EpochNo` (a bare CBOR unsigned
/// integer). Returns the epoch number on success.
pub fn decode_epoch_no(sig_cbor: &[u8]) -> Result<u64, String> {
    if sig_cbor.is_empty() {
        return Err("sig_cbor is empty".to_string());
    }
    let mut dec = minicbor::Decoder::new(sig_cbor);
    match dec.datatype().map_err(|e| format!("sig datatype: {e}"))? {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            dec.u64().map_err(|e| format!("sig epoch_no decode: {e}"))
        }
        other => Err(format!(
            "sig_cbor: expected unsigned integer, got {other:?}"
        )),
    }
}

/// Decode the initial epoch number from field [0] of a NewEpochState blob.
///
/// NewEpochState is encoded as `array(7)` where field [0] is the current
/// `EpochNo` (a bare CBOR u64).
pub fn decode_initial_epoch_no(st_cbor: &[u8]) -> Result<u64, String> {
    if st_cbor.is_empty() {
        return Err("st_cbor is empty".to_string());
    }
    let mut dec = minicbor::Decoder::new(st_cbor);

    // Outer array(7)
    match dec
        .array()
        .map_err(|e| format!("NewEpochState outer array: {e}"))?
    {
        Some(7) => {}
        Some(n) => return Err(format!("NewEpochState: expected array(7), got array({n})")),
        None => return Err("NewEpochState: expected definite array(7), got indefinite".to_string()),
    }

    // Field [0] = EpochNo (u64)
    match dec
        .datatype()
        .map_err(|e| format!("field[0] datatype: {e}"))?
    {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => dec
            .u64()
            .map_err(|e| format!("NewEpochState field[0] epoch_no: {e}")),
        other => Err(format!(
            "NewEpochState field[0]: expected unsigned integer, got {other:?}"
        )),
    }
}

/// Extract the human-readable top-level CBOR shape of `cbor`.
fn top_level_shape(cbor: &[u8], label: &str) -> Result<String, String> {
    let mut dec = minicbor::Decoder::new(cbor);
    let ty = dec
        .datatype()
        .map_err(|e| format!("{label} datatype: {e}"))?;
    match ty {
        Type::Array => {
            let len = dec.array().map_err(|e| format!("{label} array: {e}"))?;
            match len {
                Some(n) => Ok(format!("arr[{n}]")),
                None => Ok("arr[indef]".to_string()),
            }
        }
        Type::Map => {
            let len = dec.map().map_err(|e| format!("{label} map: {e}"))?;
            match len {
                Some(n) => Ok(format!("map({n})")),
                None => Ok("map(indef)".to_string()),
            }
        }
        other => Ok(format!("{other:?}")),
    }
}
