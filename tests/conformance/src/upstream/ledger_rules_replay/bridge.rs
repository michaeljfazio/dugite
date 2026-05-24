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
//!    `array(7)` NewEpochState, including all three previously-stubbed
//!    sub-trees: LedgerState, Snapshots, and NonMyopic.
//!
//! ## NewEpochState array(7) field layout (Haskell / cardano-ledger)
//!
//! ```text
//! [0] nesEL      :: EpochNo              u64
//! [1] nesBprev   :: BlocksMade           map pool_keyhash → u64
//! [2] nesBcur    :: BlocksMade           map pool_keyhash → u64
//! [3] nesEs       :: EpochState          array(4)
//!     [3.0] AccountState  :: [treasury: Coin, reserves: Coin]   array(2) u64s
//!     [3.1] LedgerState   :: array(2) = [CertState (complex), UTxOState array(6)]
//!           UTxOState = [utxo_map, deposited_coin, fees_coin, gov_state, instant_stake, donation]
//!     [3.2] EpochSnapshots:: array(4) = [mark, set, go (each SnapShot array(2|3)), fee_coin]
//!     [3.3] NonMyopic     :: array(2) = [likelihoods_map, reward_pot_coin]
//! [4] nesRu       :: StrictMaybe PulsingRewUpdate  array(0)=Nothing | array(1)=Just
//! [5] nesPd       :: PoolDistr           map (with rational total stake)
//! [6] stashedAVVM :: ()                  array(0) in Conway (always empty)
//! ```
//!
//! ## Source references (Haskell cardano-ledger, queried 2026-05-23)
//!
//! - `LedgerState` encoding: `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs`
//!   → `encCBOR LedgerState` → `encodeListLen 2 <> encCBOR lsCertState <> encCBOR lsUTxOState`
//! - `UTxOState` encoding: same file → `encCBOR UTxOState` → `encodeListLen 6`
//! - `SnapShots` encoding: `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs`
//!   → `encCBOR SnapShots` → `encodeListLen 4` (ssStakeMarkPoolDistr NOT serialized)
//! - `SnapShot` encoding: same file → `encCBOR SnapShot` → `encodeListLen 2` (new format);
//!   decoder accepts 2 (new) or 3 (old: Stake + Delegations + PoolsMap)
//! - `NonMyopic` encoding: `eras/shelley/impl/src/Cardano/Ledger/Shelley/PoolRank.hs`
//!   → `encCBOR NonMyopic` → `encodeListLen 2 <> encCBOR likelihoodsNM <> encCBOR rewardPotNM`
//!
//! ## Native STS signal decoders (Phase 4 native-signal extension)
//!
//! For the 8 non-Tx rules (POOL, CERT, CERTS, DELEG, GOVCERT, GOV, ENACT, RATIFY)
//! the signal is NOT a full `Tx era`.  The actual on-wire CBOR encoding (from
//! patched cardano-ledger ImpSpec output, verified 2026-05-24) is:
//!
//! **TxCert / STS rule signals**: a flat CBOR array whose first element is a uint
//! discriminator tag.  Observed tag values in the real ImpSpec corpus:
//!
//! | Rule    | Observed tags       | Interpretation |
//! |---------|---------------------|----------------|
//! | POOL    | 0 (PoolReg), 1 (PoolRetire) | PoolCert |
//! | DELEG   | 7–12                | ConwayDelegCert |
//! | GOVCERT | 14–18               | ConwayGovCert |
//! | CERT    | any of the above + more | TxCert (all variants) |
//!
//! **CERTS**: definite CBOR array of TxCert elements (each element is a TxCert as above).
//! **GOV**: `[map, set_or_array, list]` — VotingProcedures map + OSet + ProposalProcedures.
//! **ENACT**: `[GovActionId, GovAction]` — array(2) where GovActionId is array(2)[bstr(32), uint].
//! **RATIFY**: definite array of GovActionState elements, each array(7).
//!
//! ## Future work (Phase 4 follow-on)
//!
//! Once `dugite-ledger` exposes a public deserialization API for
//! `NewEpochState` / `LedgerState`, replace the raw-CBOR path with a full
//! typed decode so that `runner.rs` can call `Ledger::apply_tx` directly.

use minicbor::data::Type;

// ── DecodedLedgerState, DecodedSnapshots, DecodedNonMyopic ───────────────────

/// Structural decode of a `LedgerState` sub-tree (field[3.1] of EpochState).
///
/// Haskell encoding (from `Types.hs`):
/// ```text
/// encCBOR LedgerState = encodeListLen 2 <> encCBOR lsCertState <> encCBOR lsUTxOState
/// ```
/// Note: CertState is encoded FIRST (for intern sharing), UTxOState is second.
///
/// UTxOState (array(6)):
/// ```text
/// [utxo_map, deposited_coin, fees_coin, gov_state, instant_stake, donation_coin]
/// ```
#[derive(Debug, Default)]
pub struct DecodedLedgerState {
    /// Number of UTxO entries in the `utxosUtxo` map (field[3.1.1.0]).
    pub utxo_count: Option<u64>,
    /// Deposit pot in lovelace (`utxosDeposited`, field[3.1.1.1]).
    pub deposited: Option<u64>,
    /// Fee pot in lovelace (`utxosFees`, field[3.1.1.2]).
    pub fees: Option<u64>,
    /// Donation in lovelace (`utxosDonation`, field[3.1.1.5]).
    pub donation: Option<u64>,
    /// Raw CBOR byte length of the full LedgerState blob (for diagnostics).
    pub cbor_len: usize,
}

/// Structural decode of the `SnapShots` sub-tree (field[3.2] of EpochState).
///
/// Haskell encoding (from `SnapShots.hs`):
/// ```text
/// encCBOR SnapShots = encodeListLen 4
///   <> encCBOR ssStakeMark   -- mark (array(2))
///   <> encCBOR ssStakeSet    -- set  (array(2))
///   <> encCBOR ssStakeGo     -- go   (array(2))
///   <> encCBOR ssFee         -- fee  (Coin / u64)
/// ```
/// Note: `ssStakeMarkPoolDistr` is intentionally NOT serialized.
/// Each `SnapShot` is array(2) in the new format; array(3) in the old format.
#[derive(Debug, Default)]
pub struct DecodedSnapshots {
    /// Number of snapshot items decoded (should be 4: mark, set, go, fee).
    pub field_count: u64,
    /// Number of stake pools in the mark snapshot.
    pub mark_pool_count: Option<u64>,
    /// Number of stake pools in the set snapshot.
    pub set_pool_count: Option<u64>,
    /// Number of stake pools in the go snapshot.
    pub go_pool_count: Option<u64>,
    /// Fee snapshot (Coin / lovelace).
    pub fee: Option<u64>,
    /// Raw CBOR byte length of the full SnapShots blob (for diagnostics).
    pub cbor_len: usize,
}

/// Structural decode of the `NonMyopic` sub-tree (field[3.3] of EpochState).
///
/// Haskell encoding (from `PoolRank.hs`):
/// ```text
/// encCBOR NonMyopic = encodeListLen 2
///   <> encCBOR likelihoodsNM   -- VMap pool_id → Likelihood (map)
///   <> encCBOR rewardPotNM     -- Coin (u64)
/// ```
#[derive(Debug, Default)]
pub struct DecodedNonMyopic {
    /// Number of entries in the likelihoods map.
    pub likelihood_count: Option<u64>,
    /// Reward pot in lovelace (`rewardPotNM`).
    pub reward_pot: Option<u64>,
    /// Raw CBOR byte length of the full NonMyopic blob (for diagnostics).
    pub cbor_len: usize,
}

// ── DecodedNewEpochState ──────────────────────────────────────────────────────

/// Structural decode of a NewEpochState `array(7)` blob.
///
/// All seven fields are decoded, including the three previously-stubbed
/// sub-trees: LedgerState, Snapshots, and NonMyopic.
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
    /// field[3.1] — LedgerState: UTxO count, deposit pot, fees, donation.
    pub ledger_state: DecodedLedgerState,
    /// field[3.2] — EpochSnapshots: mark/set/go pool counts and fee.
    pub snapshots: DecodedSnapshots,
    /// field[3.3] — NonMyopic: likelihoods map count and reward pot.
    pub nonmyopic: DecodedNonMyopic,
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
/// All seven fields are decoded structurally, including LedgerState, Snapshots,
/// and NonMyopic sub-trees.  Deep sub-fields that are not needed for invariant
/// checking (CertState, GovState, InstantStake) are consumed via `skip()`.
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

    // ── field[3.1] LedgerState array(2): [CertState, UTxOState] ─────────────
    let before_ls = dec.position();
    let ledger_state =
        decode_ledger_state(&mut dec, "field[3.1] LedgerState").unwrap_or_else(|e| {
            // Non-fatal: if we cannot structurally decode the LedgerState
            // (e.g. novel format in a future era), record the error and
            // continue.  The epoch-invariant check is the gating check.
            eprintln!("[bridge] WARN LedgerState decode (non-fatal): {e}");
            DecodedLedgerState::default()
        });
    let ls_cbor_len = dec.position() - before_ls;
    let ledger_state = DecodedLedgerState {
        cbor_len: ls_cbor_len,
        ..ledger_state
    };

    // ── field[3.2] EpochSnapshots array(4): [mark, set, go, fee] ────────────
    let before_snap = dec.position();
    let snapshots = decode_snapshots(&mut dec, "field[3.2] SnapShots").unwrap_or_else(|e| {
        eprintln!("[bridge] WARN SnapShots decode (non-fatal): {e}");
        DecodedSnapshots::default()
    });
    let snap_cbor_len = dec.position() - before_snap;
    let snapshots = DecodedSnapshots {
        cbor_len: snap_cbor_len,
        ..snapshots
    };

    // ── field[3.3] NonMyopic array(2): [likelihoods_map, reward_pot] ─────────
    let before_nm = dec.position();
    let nonmyopic = decode_nonmyopic(&mut dec, "field[3.3] NonMyopic").unwrap_or_else(|e| {
        eprintln!("[bridge] WARN NonMyopic decode (non-fatal): {e}");
        DecodedNonMyopic::default()
    });
    let nm_cbor_len = dec.position() - before_nm;
    let nonmyopic = DecodedNonMyopic {
        cbor_len: nm_cbor_len,
        ..nonmyopic
    };

    // ── field[4] StrictMaybe PulsingRewUpdate ─────────────────────────────────
    let pulsing_rew_update = decode_strict_maybe(&mut dec, "field[4] StrictMaybe")?;

    // ── field[5] PoolDistr ────────────────────────────────────────────────────
    // Pre-Conway: bare map.  Conway: array(2)[map, CompactCoin].
    let pool_distr_count = decode_pool_distr_count(&mut dec, "field[5] PoolDistr")?;

    // ── field[6] stashedAVVM — array(0) in Conway ─────────────────────────────
    let stashed_avvm_len = decode_stashed_avvm(&mut dec, "field[6] stashedAVVM")?;

    Ok(DecodedNewEpochState {
        epoch_no,
        blocks_prev_count,
        blocks_cur_count,
        treasury,
        reserves,
        ledger_state,
        snapshots,
        nonmyopic,
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
/// Decode a PoolDistr field, handling both wire formats:
/// - Pre-Conway: bare CBOR map.
/// - Conway: `array(2)[map, CompactCoin]` (`encodeListLen 2 <> encCBOR map <> encCBOR total`).
///
/// Returns the number of pool entries in the map.
fn decode_pool_distr_count(dec: &mut minicbor::Decoder<'_>, label: &str) -> Result<u64, String> {
    match dec
        .datatype()
        .map_err(|e| format!("{label} peek type: {e}"))?
    {
        Type::Array => {
            // Conway encoding: array(2)[map, CompactCoin]
            match dec
                .array()
                .map_err(|e| format!("{label} array header: {e}"))?
            {
                Some(2) => {}
                Some(n) => {
                    return Err(format!(
                        "{label}: Conway PoolDistr should be array(2), got array({n})"
                    ))
                }
                None => {
                    return Err(format!(
                        "{label}: Conway PoolDistr should be definite array(2)"
                    ))
                }
            }
            let count = decode_map_count(dec, &format!("{label} inner map"))?;
            dec.skip()
                .map_err(|e| format!("{label} CompactCoin skip: {e}"))?;
            Ok(count)
        }
        Type::Map => {
            // Pre-Conway bare map
            decode_map_count(dec, label)
        }
        other => Err(format!(
            "{label}: expected map or array(2), got {other:?}"
        )),
    }
}

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

/// Decode a `LedgerState` `array(2)` sub-tree.
///
/// Haskell encoding order: `[CertState, UTxOState]` (CertState first for intern sharing).
///
/// UTxOState is `array(6)`:
///   `[utxo_map, deposited_coin, fees_coin, gov_state, instant_stake, donation_coin]`
///
/// CertState is a complex sub-tree that we skip entirely.
fn decode_ledger_state(
    dec: &mut minicbor::Decoder<'_>,
    label: &str,
) -> Result<DecodedLedgerState, String> {
    match dec
        .array()
        .map_err(|e| format!("{label} array header: {e}"))?
    {
        Some(2) => {}
        Some(n) => return Err(format!("{label}: expected array(2), got array({n})")),
        None => {
            return Err(format!(
                "{label}: expected definite array(2), got indefinite"
            ))
        }
    }

    // Sub-field [0]: CertState — complex (DState + PState + VState); skip entirely.
    dec.skip()
        .map_err(|e| format!("{label} CertState skip: {e}"))?;

    // Sub-field [1]: UTxOState array(6):
    //   [utxo_map, deposited_coin, fees_coin, gov_state, instant_stake, donation_coin]
    match dec
        .array()
        .map_err(|e| format!("{label} UTxOState array header: {e}"))?
    {
        Some(6) => {}
        Some(n) => {
            return Err(format!(
                "{label} UTxOState: expected array(6), got array({n})"
            ))
        }
        None => {
            return Err(format!(
                "{label} UTxOState: expected definite array(6), got indefinite"
            ))
        }
    }

    // [1.0] utxo_map: map txin → txout
    let utxo_count = Some(decode_map_count(dec, &format!("{label} UTxO map"))?);

    // [1.1] deposited_coin
    let deposited = Some(decode_u64(dec, &format!("{label} deposited"))?);

    // [1.2] fees_coin
    let fees = Some(decode_u64(dec, &format!("{label} fees"))?);

    // [1.3] gov_state (GovState — complex Conway gov state); skip entirely.
    dec.skip()
        .map_err(|e| format!("{label} GovState skip: {e}"))?;

    // [1.4] instant_stake — skip entirely.
    dec.skip()
        .map_err(|e| format!("{label} InstantStake skip: {e}"))?;

    // [1.5] donation_coin
    let donation = Some(decode_u64(dec, &format!("{label} donation"))?);

    Ok(DecodedLedgerState {
        utxo_count,
        deposited,
        fees,
        donation,
        cbor_len: 0, // filled in by caller
    })
}

/// Decode a `SnapShots` `array(4)` sub-tree.
///
/// Haskell encoding:
/// ```text
/// encodeListLen 4 <> encCBOR ssStakeMark <> encCBOR ssStakeSet <> encCBOR ssStakeGo <> encCBOR ssFee
/// ```
/// Note: `ssStakeMarkPoolDistr` is intentionally NOT serialized.
///
/// Each `SnapShot` is `array(2)` (new format) or `array(3)` (old format).
/// New: `[ActiveStake, StakePoolsMap]`; Old: `[Stake, Delegations, StakePoolsMap]`.
/// We only need the pool count, which is the last map in each snapshot.
fn decode_snapshots(
    dec: &mut minicbor::Decoder<'_>,
    label: &str,
) -> Result<DecodedSnapshots, String> {
    match dec
        .array()
        .map_err(|e| format!("{label} array header: {e}"))?
    {
        Some(4) => {}
        Some(n) => return Err(format!("{label}: expected array(4), got array({n})")),
        None => {
            return Err(format!(
                "{label}: expected definite array(4), got indefinite"
            ))
        }
    }

    let mark_pool_count = decode_snapshot_pool_count(dec, &format!("{label}[mark]")).ok();
    let set_pool_count = decode_snapshot_pool_count(dec, &format!("{label}[set]")).ok();
    let go_pool_count = decode_snapshot_pool_count(dec, &format!("{label}[go]")).ok();

    // ssFee: Coin (u64)
    let fee = decode_u64(dec, &format!("{label} fee")).ok();

    Ok(DecodedSnapshots {
        field_count: 4,
        mark_pool_count,
        set_pool_count,
        go_pool_count,
        fee,
        cbor_len: 0, // filled in by caller
    })
}

/// Decode a single `SnapShot` and return the number of stake pools in its map.
///
/// SnapShot is `array(2)` or `array(3)`:
/// - New (2): `[ActiveStake, StakePoolsSnapShotMap]`
/// - Old (3): `[Stake, Delegations, StakePoolsSnapShotMap]`
///
/// We skip leading fields and count entries in the final StakePoolsSnapShotMap.
fn decode_snapshot_pool_count(dec: &mut minicbor::Decoder<'_>, label: &str) -> Result<u64, String> {
    let n = match dec
        .array()
        .map_err(|e| format!("{label} SnapShot array header: {e}"))?
    {
        Some(n) => n,
        None => {
            return Err(format!(
                "{label}: expected definite array for SnapShot, got indefinite"
            ))
        }
    };

    match n {
        2 => {
            // New format: [ActiveStake, StakePoolsMap]
            // ActiveStake is a complex VMap; skip it.
            dec.skip()
                .map_err(|e| format!("{label} ActiveStake skip: {e}"))?;
        }
        3 => {
            // Old format: [Stake, Delegations, StakePoolsMap]
            dec.skip().map_err(|e| format!("{label} Stake skip: {e}"))?;
            dec.skip()
                .map_err(|e| format!("{label} Delegations skip: {e}"))?;
        }
        _ => {
            return Err(format!(
                "{label}: expected SnapShot array(2) or array(3), got array({n})"
            ))
        }
    }

    // StakePoolsSnapShotMap: map pool_id → StakePoolSnapShot
    let pool_count = decode_map_count(dec, &format!("{label} StakePools map"))?;
    Ok(pool_count)
}

/// Decode a `NonMyopic` `array(2)` sub-tree.
///
/// Haskell encoding:
/// ```text
/// encodeListLen 2 <> encCBOR likelihoodsNM <> encCBOR rewardPotNM
/// ```
fn decode_nonmyopic(
    dec: &mut minicbor::Decoder<'_>,
    label: &str,
) -> Result<DecodedNonMyopic, String> {
    match dec
        .array()
        .map_err(|e| format!("{label} array header: {e}"))?
    {
        Some(2) => {}
        Some(n) => return Err(format!("{label}: expected array(2), got array({n})")),
        None => {
            return Err(format!(
                "{label}: expected definite array(2), got indefinite"
            ))
        }
    }

    // [0] likelihoodsNM: VMap pool_id → Likelihood (map)
    let likelihood_count = Some(decode_map_count(dec, &format!("{label} likelihoods map"))?);

    // [1] rewardPotNM: Coin (u64)
    let reward_pot = Some(decode_u64(dec, &format!("{label} rewardPot"))?);

    Ok(DecodedNonMyopic {
        likelihood_count,
        reward_pot,
        cbor_len: 0, // filled in by caller
    })
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

// ── Native STS signal decoders ────────────────────────────────────────────────
//
// The ImpSpec corpus uses rule-specific signal types for POOL, CERT, CERTS,
// DELEG, GOVCERT, GOV, ENACT, and RATIFY — none of these are full `Tx era`
// blobs.  The decoders below provide structural validation matching the actual
// on-wire encoding observed in the real ImpSpec corpus (verified 2026-05-24).
//
// Tag values (from the real corpus):
//   POOL:    [0, pool_params_list]       = PoolRegistration
//            [1, pool_id_bstr28, epoch]  = PoolRetirement
//   DELEG:   tags 7–12  (ConwayDelegCert variants)
//   GOVCERT: tags 14–18 (ConwayGovCert variants)
//   CERT:    any tag seen in POOL ∪ DELEG ∪ GOVCERT (plus additional variants)
//
// The encoding for GOV, ENACT, and RATIFY differs from TxCert:
//   GOV:    [map, set/array, list]   (VotingProcedures, OSet, ProposalProcedures)
//   ENACT:  [[bstr(32), uint], govaction] — array(2)
//   RATIFY: array of array(7)

/// Validate a raw CBOR blob as a single TxCert signal.
///
/// The ImpSpec TxCert signal is a flat CBOR array whose first element is a
/// uint discriminator tag.  We do not validate the tag range here — that is
/// done in the per-rule wrappers (`decode_pool_signal`, etc.).
///
/// Returns the leading uint tag on success.
pub fn decode_tx_cert_tag(cbor: &[u8]) -> Result<u64, String> {
    if cbor.is_empty() {
        return Err("TxCert signal: empty CBOR".to_string());
    }
    let mut dec = minicbor::Decoder::new(cbor);

    // Must be a definite array.
    let n = match dec
        .array()
        .map_err(|e| format!("TxCert: array header: {e}"))?
    {
        Some(n) => n,
        None => return Err("TxCert: expected definite array, got indefinite".to_string()),
    };

    if n == 0 {
        return Err("TxCert: empty array (no tag field)".to_string());
    }

    // First element is the uint discriminator tag.
    match dec
        .datatype()
        .map_err(|e| format!("TxCert: tag datatype: {e}"))?
    {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            dec.u64().map_err(|e| format!("TxCert: tag decode: {e}"))
        }
        other => Err(format!(
            "TxCert: expected uint tag as first array element, got {other:?}"
        )),
    }
}

/// Validate a POOL signal: `[0, pool_params_list]` or `[1, pool_id, epoch]`.
///
/// Observed POOL tags in the real corpus: 0 (PoolRegistration), 1 (PoolRetirement).
pub fn decode_pool_signal(cbor: &[u8]) -> Result<u64, String> {
    let tag = decode_tx_cert_tag(cbor)?;
    // Tags 0 and 1 are the only valid POOL cert tags observed in the real corpus.
    if tag == 0 || tag == 1 {
        Ok(tag)
    } else {
        Err(format!(
            "POOL signal: unexpected tag {tag} (expected 0=PoolReg or 1=PoolRetire)"
        ))
    }
}

/// Validate a DELEG signal: a ConwayDelegCert with tags 7–12.
///
/// Observed DELEG tags in the real corpus: 7, 8, 9, 11, 12.
pub fn decode_deleg_signal(cbor: &[u8]) -> Result<u64, String> {
    let tag = decode_tx_cert_tag(cbor)?;
    // All observed DELEG tags are in the range 7–12.  We allow any value in
    // that range — specific variants not yet observed in the corpus (e.g. 10)
    // may appear in future fixture generations.
    if (7..=12).contains(&tag) {
        Ok(tag)
    } else {
        Err(format!(
            "DELEG signal: unexpected tag {tag} (expected 7–12 for ConwayDelegCert)"
        ))
    }
}

/// Validate a GOVCERT signal: a ConwayGovCert with tags 14–18.
///
/// Observed GOVCERT tags in the real corpus: 14, 15, 16, 18.
pub fn decode_govcert_signal(cbor: &[u8]) -> Result<u64, String> {
    let tag = decode_tx_cert_tag(cbor)?;
    // All observed GOVCERT tags are in the range 14–18.
    if (14..=18).contains(&tag) {
        Ok(tag)
    } else {
        Err(format!(
            "GOVCERT signal: unexpected tag {tag} (expected 14–18 for ConwayGovCert)"
        ))
    }
}

/// Validate a CERT signal: any TxCert variant.
///
/// CERT is the union of POOL, DELEG, and GOVCERT, so any valid tag is accepted.
/// Observed CERT tags in the real corpus: 3, 7, 8, 11, 12, 18.
pub fn decode_cert_signal(cbor: &[u8]) -> Result<u64, String> {
    // Any uint tag is valid for CERT — it is the superset of all cert variants.
    decode_tx_cert_tag(cbor)
}

/// Validate a CERTS signal: a definite CBOR array of TxCert elements.
///
/// Returns the count of TxCert elements (may be 0 for empty sequences).
pub fn decode_certs_signal_count(cbor: &[u8]) -> Result<usize, String> {
    if cbor.is_empty() {
        return Err("CERTS signal: empty CBOR".to_string());
    }
    let mut dec = minicbor::Decoder::new(cbor);

    // Outer: definite array of TxCert elements.
    let n = match dec
        .array()
        .map_err(|e| format!("CERTS: outer array header: {e}"))?
    {
        Some(n) => n,
        None => return Err("CERTS signal: expected definite array, got indefinite".to_string()),
    };

    // Validate each element as a TxCert (flat array with uint tag).
    for i in 0..n {
        // Each TxCert is itself a definite array.
        let elem_len = match dec
            .array()
            .map_err(|e| format!("CERTS element[{i}]: array header: {e}"))?
        {
            Some(len) => len,
            None => {
                return Err(format!(
                    "CERTS element[{i}]: expected definite array, got indefinite"
                ))
            }
        };
        if elem_len == 0 {
            return Err(format!("CERTS element[{i}]: empty TxCert array (no tag)"));
        }
        // First element must be a uint tag.
        match dec
            .datatype()
            .map_err(|e| format!("CERTS element[{i}] tag datatype: {e}"))?
        {
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                dec.u64()
                    .map_err(|e| format!("CERTS element[{i}] tag: {e}"))?;
            }
            other => {
                return Err(format!(
                    "CERTS element[{i}]: expected uint tag, got {other:?}"
                ))
            }
        }
        // Skip remaining fields of the TxCert.
        for j in 1..elem_len {
            dec.skip()
                .map_err(|e| format!("CERTS element[{i}] field[{j}]: {e}"))?;
        }
    }

    Ok(n as usize)
}

/// Validate a GOV signal: `[VotingProcedures, OSet, ProposalProcedures]`.
///
/// The GOV signal is `GovSignal era` encoded as a 3-element definite array.
/// Based on the real ImpSpec corpus (verified 2026-05-24):
///   [0] VotingProcedures — a CBOR map
///   [1] OSet             — a CBOR set (OSet ProposalProcedure)
///   [2] ProposalProcedures — a CBOR array of proposal procedures
///
/// The third field in the real corpus is an array, not a Coin uint.
/// We validate the outer shape (array(3)) and that field[0] is a map;
/// fields [1] and [2] are skipped entirely.
pub fn decode_gov_signal_shape(cbor: &[u8]) -> Result<(), String> {
    if cbor.is_empty() {
        return Err("GOV signal: empty CBOR".to_string());
    }
    let mut dec = minicbor::Decoder::new(cbor);

    // Outer: definite array(3).
    match dec
        .array()
        .map_err(|e| format!("GOV: outer array header: {e}"))?
    {
        Some(3) => {}
        Some(n) => return Err(format!("GOV signal: expected array(3), got array({n})")),
        None => return Err("GOV signal: expected definite array(3), got indefinite".to_string()),
    }

    // [0] VotingProcedures — must be a map (possibly empty).
    match dec
        .datatype()
        .map_err(|e| format!("GOV field[0] datatype: {e}"))?
    {
        Type::Map => {
            dec.skip()
                .map_err(|e| format!("GOV field[0] VotingProcedures skip: {e}"))?;
        }
        other => {
            return Err(format!(
                "GOV field[0]: expected map (VotingProcedures), got {other:?}"
            ))
        }
    }

    // [1] OSet ProposalProcedure — may be a CBOR set or tagged array.
    //     Skip entirely.
    dec.skip()
        .map_err(|e| format!("GOV field[1] OSet skip: {e}"))?;

    // [2] ProposalProcedures — in the real corpus this is an array (not a Coin).
    //     Accept any encoding — skip entirely.
    dec.skip()
        .map_err(|e| format!("GOV field[2] ProposalProcedures skip: {e}"))?;

    Ok(())
}

/// Validate an ENACT signal: `[GovActionId, GovAction]`.
///
/// The ENACT signal is `EnactSignal era` = `(GovActionId, GovAction)`.
/// On wire it is a 2-element definite array:
///   [0] GovActionId — array(2)[bstr(32), uint]
///   [1] GovAction   — array with leading uint tag
pub fn decode_enact_signal_shape(cbor: &[u8]) -> Result<(), String> {
    if cbor.is_empty() {
        return Err("ENACT signal: empty CBOR".to_string());
    }
    let mut dec = minicbor::Decoder::new(cbor);

    // Outer: definite array(2).
    match dec
        .array()
        .map_err(|e| format!("ENACT: outer array header: {e}"))?
    {
        Some(2) => {}
        Some(n) => return Err(format!("ENACT signal: expected array(2), got array({n})")),
        None => return Err("ENACT signal: expected definite array(2), got indefinite".to_string()),
    }

    // [0] GovActionId — must be array(2)[bstr(32), uint].
    match dec
        .array()
        .map_err(|e| format!("ENACT field[0] GovActionId array header: {e}"))?
    {
        Some(2) => {}
        Some(n) => {
            return Err(format!(
                "ENACT field[0] GovActionId: expected array(2), got array({n})"
            ))
        }
        None => {
            return Err(
                "ENACT field[0] GovActionId: expected definite array(2), got indefinite"
                    .to_string(),
            )
        }
    }
    // [0.0] tx hash bstr(32)
    match dec
        .datatype()
        .map_err(|e| format!("ENACT GovActionId[0] datatype: {e}"))?
    {
        Type::Bytes => {
            let bs = dec
                .bytes()
                .map_err(|e| format!("ENACT GovActionId[0] bytes: {e}"))?;
            if bs.len() != 32 {
                return Err(format!(
                    "ENACT GovActionId[0]: expected 32-byte tx hash, got {} bytes",
                    bs.len()
                ));
            }
        }
        other => {
            return Err(format!(
                "ENACT GovActionId[0]: expected bstr(32) tx hash, got {other:?}"
            ))
        }
    }
    // [0.1] action index (uint)
    match dec
        .datatype()
        .map_err(|e| format!("ENACT GovActionId[1] datatype: {e}"))?
    {
        Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
            dec.skip()
                .map_err(|e| format!("ENACT GovActionId[1] index skip: {e}"))?;
        }
        other => {
            return Err(format!(
                "ENACT GovActionId[1]: expected uint action index, got {other:?}"
            ))
        }
    }

    // [1] GovAction — skip entirely (complex, many variants).
    dec.skip()
        .map_err(|e| format!("ENACT field[1] GovAction skip: {e}"))?;

    Ok(())
}

/// Validate a RATIFY signal: an array of `GovActionState` elements.
///
/// Each `GovActionState` is encoded as `array(7)`.  An empty RATIFY signal
/// (no proposals to ratify) is encoded as `array(0)` = `0x80`.  Large
/// signals may use an indefinite-length outer array (`0x9f...0xff`).
///
/// Returns the number of `GovActionState` elements (may be 0).
pub fn decode_ratify_signal_count(cbor: &[u8]) -> Result<usize, String> {
    if cbor.is_empty() {
        return Err("RATIFY signal: empty CBOR".to_string());
    }
    let mut dec = minicbor::Decoder::new(cbor);

    // Outer: definite or indefinite array of GovActionState elements.
    let definite_len = dec
        .array()
        .map_err(|e| format!("RATIFY: outer array header: {e}"))?;

    match definite_len {
        Some(n) => {
            // Definite array: validate each element is array(7).
            for i in 0..n {
                match dec
                    .array()
                    .map_err(|e| format!("RATIFY element[{i}]: array header: {e}"))?
                {
                    Some(7) => {
                        for j in 0..7u64 {
                            dec.skip()
                                .map_err(|e| format!("RATIFY element[{i}] field[{j}]: {e}"))?;
                        }
                    }
                    Some(m) => {
                        return Err(format!(
                            "RATIFY element[{i}]: expected array(7) GovActionState, got array({m})"
                        ))
                    }
                    None => {
                        return Err(format!(
                            "RATIFY element[{i}]: expected definite array(7), got indefinite"
                        ))
                    }
                }
            }
            Ok(n as usize)
        }
        None => {
            // Indefinite array: walk until break, counting elements.
            let mut count = 0usize;
            loop {
                match dec
                    .datatype()
                    .map_err(|e| format!("RATIFY indef: datatype at element {count}: {e}"))?
                {
                    Type::Break => {
                        dec.skip().map_err(|e| format!("RATIFY indef break: {e}"))?;
                        break;
                    }
                    Type::Array => {
                        // Each element must be array(7).
                        match dec
                            .array()
                            .map_err(|e| format!("RATIFY element[{count}]: array header: {e}"))?
                        {
                            Some(7) => {
                                for j in 0..7u64 {
                                    dec.skip().map_err(|e| {
                                        format!("RATIFY element[{count}] field[{j}]: {e}")
                                    })?;
                                }
                            }
                            Some(m) => {
                                return Err(format!(
                                    "RATIFY element[{count}]: expected array(7) GovActionState, got array({m})"
                                ))
                            }
                            None => {
                                return Err(format!(
                                    "RATIFY element[{count}]: expected definite array(7), got indefinite"
                                ))
                            }
                        }
                    }
                    other => {
                        return Err(format!(
                            "RATIFY indef element[{count}]: expected array or break, got {other:?}"
                        ))
                    }
                }
                count += 1;
            }
            Ok(count)
        }
    }
}
