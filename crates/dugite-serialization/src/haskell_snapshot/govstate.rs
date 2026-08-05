//! ConwayGovState decoder for Haskell ledger snapshots.
//!
//! Haskell serialises `ConwayGovState` as a flat CBOR `array(7)` in this
//! fixed positional order:
//!
//! ```text
//! ConwayGovState = array(7) [
//!   [0] proposals:        complex structure — captured as raw CBOR bytes
//!   [1] committee:        StrictMaybe(Committee) — captured as raw CBOR bytes
//!   [2] constitution:     array(2) [Anchor, ScriptHash]
//!                           Anchor     = array(2) [url_text, bytes(32)]
//!                           ScriptHash = bytes(28) — direct bytestring, NOT wrapped
//!   [3] curPParams:       array(31) — decoded via decode_pparams
//!   [4] prevPParams:      array(31) — decoded via decode_pparams
//!   [5] futurePParams:    tagged sum —
//!                           array(1)[0]              = NoPParamsUpdate
//!                           array(2)[1, pp]          = DefinitePParamsUpdate(pp)
//!                           array(2)[2, array(0)]    = PotentialPParamsUpdate(SNothing)
//!                           array(2)[2, array(1)[pp]]= PotentialPParamsUpdate(SJust(pp))
//!   [6] drepPulsingState: complex structure — captured as raw CBOR bytes
//! ]
//! ```
//!
//! `cur_pparams` and `prev_pparams` are decoded into `ProtocolParameters`
//! and stored directly on `HaskellGovState` so the top-level decoder can
//! copy them into `HaskellNewEpochState` without re-decoding.
//!
//! All other complex sub-structures are preserved verbatim as raw CBOR bytes
//! so they can be decoded on-demand or passed through to consumers that need
//! the full fidelity wire format.

use crate::error::SerializationError;
use dugite_primitives::hash::Hash28;
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::time::EpochNo;

use super::cbor_utils::{
    bounded_alloc_capacity, decode_array_len, decode_bytes, decode_credential, decode_hash32,
    decode_map_len, decode_rational, decode_text, decode_uint, skip_cbor_value,
};
use super::pparams::decode_pparams;
use super::types::{
    HaskellConstitution, HaskellGovActionId, HaskellGovActionState, HaskellGovState, HaskellVote,
};

/// Maximum constitutional committee members we accept on snapshot decode.
/// CIP-1694 caps the committee at `committeeMaxTermLength`-many members; the
/// largest realistic value is in the hundreds. 4096 is generous headroom.
///
/// #554: prevents allocation-bomb attacks via tampered gov-state.
const MAX_COMMITTEE_MEMBERS: usize = 4096;

// ── Public entry point ──────────────────────────────────────────────────────

/// Decode a `ConwayGovState` from a CBOR `array(7)`.
///
/// Returns `(HaskellGovState, bytes_consumed)`.
///
/// `HaskellGovState::cur_pparams` and `::prev_pparams` are fully decoded
/// from positions [3] and [4] of the array.  Positions [0], [1], and [6] are
/// preserved verbatim as raw CBOR byte vectors.
pub fn decode_govstate(data: &[u8]) -> Result<(HaskellGovState, usize), SerializationError> {
    let mut off = 0;

    // ── outer array(7) header ───────────────────────────────────────────────
    let (arr_len, n) = decode_array_len(&data[off..])?;
    off += n;
    if arr_len != 7 {
        return Err(SerializationError::CborDecode(format!(
            "ConwayGovState: expected array(7), got array({arr_len})"
        )));
    }

    // ── [0] proposals — capture raw CBOR bytes ──────────────────────────────
    // The proposals structure is complex (ordered map of governance action IDs
    // to GovAction/Vote bundles); we preserve it for on-demand decoding.
    let proposals_start = off;
    let proposals_size = skip_cbor_value(&data[off..])?;
    let proposals_raw = data[proposals_start..proposals_start + proposals_size].to_vec();
    off += proposals_size;

    // ── [1] committee — StrictMaybe(Committee), capture raw CBOR bytes ──────
    // Haskell StrictMaybe encoding:
    //   SNothing → array(0)  = 0x80
    //   SJust x  → array(1) [x]
    // We capture the inner content (the Committee value) if present, stripping
    // the StrictMaybe wrapper since the wrapper itself is redundant overhead.
    let committee_raw = decode_strict_maybe_raw(&data[off..])?;
    let committee_size = skip_cbor_value(&data[off..])?;
    off += committee_size;

    // ── [2] constitution — array(2) [Anchor, ScriptHash] ────────────────────
    let (constitution, n) = decode_constitution(&data[off..])?;
    off += n;

    // ── [3] curPParams — array(31) ──────────────────────────────────────────
    let (cur_pparams, n) = decode_pparams(&data[off..])?;
    off += n;

    // ── [4] prevPParams — array(31) ─────────────────────────────────────────
    let (prev_pparams, n) = decode_pparams(&data[off..])?;
    off += n;

    // ── [5] futurePParams — tagged sum ──────────────────────────────────────
    let ((future_pparams_tag, future_pparams), n) = decode_future_pparams(&data[off..])?;
    off += n;

    // ── [6] drepPulsingState — capture raw CBOR bytes ───────────────────────
    // The DRep pulsing state encodes the incremental reward calculation in
    // progress; it is large (~1.3 MB on preview) and decoded separately if
    // needed.
    let drep_start = off;
    let drep_size = skip_cbor_value(&data[off..])?;
    let drep_pulsing_raw = data[drep_start..drep_start + drep_size].to_vec();
    off += drep_size;

    Ok((
        HaskellGovState {
            proposals_raw,
            committee_raw,
            constitution,
            cur_pparams,
            prev_pparams,
            future_pparams_tag,
            future_pparams,
            drep_pulsing_raw,
        },
        off,
    ))
}

// ── Internal helpers ────────────────────────────────────────────────────────

/// Decode a Haskell `StrictMaybe T` where T is any CBOR value, returning the
/// inner raw CBOR bytes if `SJust`, or `None` if `SNothing`.
///
/// Encoding:
///   SNothing → `array(0)`   = `[0x80]`
///   SJust x  → `array(1) [x]`
///
/// The returned `Vec<u8>` is the raw CBOR of the inner value only (not
/// including the wrapping array header), so callers get the Committee bytes
/// directly rather than the StrictMaybe wrapper.
fn decode_strict_maybe_raw(data: &[u8]) -> Result<Option<Vec<u8>>, SerializationError> {
    let (arr_len, hdr) = decode_array_len(data)?;
    match arr_len {
        // SNothing — nothing to capture.
        0 => Ok(None),
        // SJust — the inner value starts immediately after the array header.
        1 => {
            let inner_size = skip_cbor_value(&data[hdr..])?;
            Ok(Some(data[hdr..hdr + inner_size].to_vec()))
        }
        n => Err(SerializationError::CborDecode(format!(
            "StrictMaybe: expected array(0) or array(1), got array({n})"
        ))),
    }
}

/// Decode a `Constitution` value:
/// ```text
/// array(2) [
///   Anchor     = array(2) [text_url, bytes(32)],
///   ScriptHash = bytes(28)          -- direct bytestring (not StrictMaybe)
/// ]
/// ```
///
/// The anchor hash is 32 bytes; the script hash is 28 bytes.  On-chain,
/// if no script hash guard is associated, the constitution can still carry
/// a default hash (the wire format in preview epoch 1259 always includes a
/// 28-byte script hash directly, not wrapped in a StrictMaybe).
///
/// Returns `(Option<HaskellConstitution>, bytes_consumed)`.  A `None` is
/// returned only if the outer array is empty; in practice this value is
/// always present after the Conway hard fork.
fn decode_constitution(
    data: &[u8],
) -> Result<(Option<HaskellConstitution>, usize), SerializationError> {
    let mut off = 0;

    let (arr_len, n) = decode_array_len(&data[off..])?;
    off += n;

    // An empty array encodes an absent constitution (pre-Conway or initial).
    if arr_len == 0 {
        return Ok((None, off));
    }

    if arr_len != 2 {
        return Err(SerializationError::CborDecode(format!(
            "Constitution: expected array(0) or array(2), got array({arr_len})"
        )));
    }

    // ── Anchor = array(2) [text_url, bytes(32)] ─────────────────────────────
    let (anchor_arr_len, n) = decode_array_len(&data[off..])?;
    off += n;
    if anchor_arr_len != 2 {
        return Err(SerializationError::CborDecode(format!(
            "Constitution Anchor: expected array(2), got array({anchor_arr_len})"
        )));
    }

    let (url_str, n) = decode_text(&data[off..])?;
    let anchor_url = url_str.to_owned();
    off += n;

    let (anchor_hash, n) = decode_hash32(&data[off..])?;
    off += n;

    // ── ScriptHash = bytes(28) ───────────────────────────────────────────────
    // In the Haskell wire format for Conway ConwayGovState, the script hash is
    // encoded as a direct bytestring — NOT wrapped in a StrictMaybe array.
    // Verified against preview epoch 1259 fixture.
    let script_hash = decode_optional_script_hash(&data[off..])?;
    let sh_size = skip_cbor_value(&data[off..])?;
    off += sh_size;

    Ok((
        Some(HaskellConstitution {
            anchor_url,
            anchor_hash,
            script_hash,
        }),
        off,
    ))
}

/// Decode the optional script hash field in a Constitution.
///
/// Haskell encodes this as a direct CBOR `bytes(28)` bytestring (not wrapped
/// in a StrictMaybe).  If the major type is not 2 (bytestring) the field is
/// treated as absent and `None` is returned — this guards against snapshots
/// from future protocol versions that might change the encoding.
fn decode_optional_script_hash(data: &[u8]) -> Result<Option<Hash28>, SerializationError> {
    if data.is_empty() {
        return Ok(None);
    }
    let major = data[0] >> 5;
    // Major type 2 = bytestring; 28 bytes → Hash28.
    if major != 2 {
        return Ok(None);
    }
    let (bytes, _) = decode_bytes(data)?;
    if bytes.len() != 28 {
        return Err(SerializationError::InvalidLength {
            expected: 28,
            got: bytes.len(),
        });
    }
    Ok(Some(Hash28::from_bytes(bytes.try_into().unwrap())))
}

/// Decode the `FuturePParams` tagged sum.
///
/// Haskell encoding (verified against preview epoch 1259):
/// ```text
/// NoPParamsUpdate              → array(1) [0]
/// DefinitePParamsUpdate pp     → array(2) [1, pp]
/// PotentialPParamsUpdate sm    → array(2) [2, StrictMaybe(pp)]
///   where sm = SNothing        → array(0)
///         sm = SJust pp        → array(1) [pp]
/// ```
///
/// Returns `((tag, Option<ProtocolParameters>), bytes_consumed)` where
/// `tag` encodes the variant:
///   - 0 = NoPParamsUpdate
///   - 1 = DefinitePParamsUpdate
///   - 2 = PotentialPParamsUpdate
fn decode_future_pparams(
    data: &[u8],
) -> Result<((u8, Option<ProtocolParameters>), usize), SerializationError> {
    let mut off = 0;

    let (arr_len, n) = decode_array_len(&data[off..])?;
    off += n;

    // Read the variant tag (always a uint).
    let (tag, n) = decode_uint(&data[off..])?;
    off += n;

    match (arr_len, tag) {
        // NoPParamsUpdate = array(1) [0]
        (1, 0) => Ok(((0, None), off)),

        // DefinitePParamsUpdate = array(2) [1, pp]
        (2, 1) => {
            let (pp, n) = decode_pparams(&data[off..])?;
            off += n;
            Ok(((1, Some(pp)), off))
        }

        // PotentialPParamsUpdate = array(2) [2, StrictMaybe(pp)]
        (2, 2) => {
            // Decode the inner StrictMaybe.
            let (inner_arr_len, n) = decode_array_len(&data[off..])?;
            off += n;
            match inner_arr_len {
                // SNothing — no future PParams queued.
                0 => Ok(((2, None), off)),
                // SJust pp — a potential future PParams update.
                1 => {
                    let (pp, n) = decode_pparams(&data[off..])?;
                    off += n;
                    Ok(((2, Some(pp)), off))
                }
                n => Err(SerializationError::CborDecode(format!(
                    "FuturePParams: PotentialPParamsUpdate StrictMaybe: \
                     expected array(0) or array(1), got array({n})"
                ))),
            }
        }

        _ => Err(SerializationError::CborDecode(format!(
            "FuturePParams: unexpected array({arr_len}) tag {tag}"
        ))),
    }
}

/// One decoded Constitutional Committee member: cold credential and expiry epoch.
///
/// `cold_tag` is the Haskell credential discriminator: `0` = KeyHash, `1` = ScriptHash.
/// `cold_hash` is the 28-byte Blake2b-224 cold credential hash.
/// `expiry` is the epoch at which this member's seat expires (inclusive).
pub type CommitteeMember = ((u8, Hash28), u64);

/// Decoded `Committee era` from a Haskell ledger snapshot.
///
/// Returned by [`decode_committee`].  The loader uses `members` to populate
/// `committee_expiration` (the canonical member list) and `threshold` to
/// populate `committee_threshold` on the dugite ledger state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HaskellCommittee {
    /// Cold credential → expiry epoch.  Preserves CBOR map iteration order
    /// for deterministic equality in tests; downstream callers iterate as a
    /// set so ordering is not load-bearing.
    pub members: Vec<CommitteeMember>,
    /// Voting threshold encoded as a `UnitInterval` rational `(num, den)`.
    pub threshold: (u64, u64),
}

/// Decode the `Committee era` value previously captured as raw CBOR bytes in
/// `HaskellGovState::committee_raw`.  The StrictMaybe wrapper has already been
/// stripped by [`decode_strict_maybe_raw`] — input here is the inner
/// `Committee` value.
///
/// Wire format (verified against IntersectMBO/cardano-ledger
/// `Conway/Governance/Procedures.hs:560-595` for Conway / PV10 / PV11 / PV12):
///
/// ```text
/// Committee era = array(2) [
///   committeeMembers   = map(N) { array(2) [uint(0|1), bytes(28)] => uint },
///   committeeThreshold = tag(30) array(2) [uint, uint]   -- UnitInterval (num, den)
/// ]
/// ```
///
/// Credential tag `0` = `KeyHashObj`, `1` = `ScriptHashObj`.  `EpochNo` is a
/// plain unsigned int.  `UnitInterval` is the standard tag-30 rational shared
/// with all other ratio fields in the ledger.
///
/// Returns `(HaskellCommittee, bytes_consumed)` so callers that decode within
/// a larger CBOR stream can advance their cursor; production callers consume
/// the whole `committee_raw` slice and ignore the length.
/// Maximum proposals we accept on snapshot decode. CIP-1694 imposes no
/// hard cap, but `govActionLifetime` × max-per-block bounds the practical
/// maximum at low thousands. 16384 is generous headroom; we cap to
/// prevent allocation-bomb attacks via tampered gov-state.
const MAX_PROPOSALS: usize = 16384;

/// Decode the Haskell `Proposals` raw CBOR captured from `proposals_raw`
/// (position [0] of the `ConwayGovState array(7)` wrapper).
///
/// Haskell encodes `Proposals` via `encCBOR proposalsActions` where
/// `proposalsActions :: Proposals era -> StrictSeq (GovActionState era)`.
/// `StrictSeq` encodes as a CBOR list, so the raw bytes are
/// `array(N) [gas_0, gas_1, …, gas_{N-1}]` where each `gas_i` is a
/// `GovActionState` `array(7)`:
///
/// ```text
/// GovActionState = array(7) [
///   gas_id            : array(2) [tx_hash(32), gov_action_index(uint)]
///   gas_committee_votes: { committee_credential => vote }
///   gas_drep_votes     : { drep_credential => vote }
///   gas_pool_votes     : { pool_key_hash(28) => vote }
///   gas_proposal_proc  : array(4) [deposit, return_addr, gov_action, anchor]
///   gas_proposed_in    : uint  (EpochNo)
///   gas_expires_after  : uint  (EpochNo)
/// ]
///
/// vote = uint  -- 0 = No, 1 = Yes, 2 = Abstain
/// ```
///
/// Returns the decoded `GovActionState`s in the order they appeared in
/// the CBOR (which is also the order Haskell tracks them via the OMap).
/// Reuses `crates/dugite-serialization/src/decode/era_conway.rs` for the
/// complex `ProposalProcedure` substructure (`Anchor`, `GovAction`,
/// `gov_action_id`) so the tx-body and snapshot decoders stay aligned.
/// The last-enacted governance action id per action purpose — Haskell's
/// `GovRelation StrictMaybe`, obtained from `toPrevGovActionIds (pRoots ps)`.
///
/// These are the `prevGovActionId` values a new proposal of each purpose must
/// chain onto. They are NOT derivable from the set of active proposals: a
/// purpose's root records the last action of that purpose ever *enacted*, which
/// on a long-lived chain is usually far older than any in-flight proposal (and
/// may have no live descendants at all).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HaskellGovRoots {
    /// `grPParamUpdate` — wire position [0].
    pub pparam_update: Option<HaskellGovActionId>,
    /// `grHardFork` — wire position [1].
    pub hard_fork: Option<HaskellGovActionId>,
    /// `grCommittee` — wire position [2].
    pub committee: Option<HaskellGovActionId>,
    /// `grConstitution` — wire position [3].
    pub constitution: Option<HaskellGovActionId>,
}

/// Haskell `Proposals`: the enacted roots plus the active proposal list.
#[derive(Debug, Default)]
pub struct HaskellProposals {
    pub roots: HaskellGovRoots,
    pub actions: Vec<HaskellGovActionState>,
}

/// Decode the Haskell `Proposals` raw CBOR, returning BOTH the enacted roots
/// and the active proposals.
///
/// There is deliberately NO roots-dropping variant of this function:
/// dropping the roots leaves every `enacted_*` root at `None`, which makes the
/// GOV rule reject (silently drop) every subsequent proposal that legitimately
/// chains onto a real enacted root. Issue #898: on preview that dropped an
/// `UpdateCommittee` proposal, so its 1000-ADA deposit was never refunded to
/// the return account, whose snapshot stake then stayed 1_000_000_000 lovelace
/// below Haskell's — depressing `totalActiveStake`, every pool's `appPerf`, and
/// ultimately every reward until an exact-drain withdrawal failed and chain
/// advance halted permanently.
pub fn decode_proposals_with_roots(data: &[u8]) -> Result<HaskellProposals, SerializationError> {
    use crate::decode::reader::Reader;

    let mut r = Reader::new(data);

    // ── Outer `array(2) [roots, omap]` wrapper ──────────────────────────
    //
    // Haskell encodes `Proposals` as a 2-tuple:
    //   `encCBOR (toPrevGovActionIds pRoots, pProps)`
    // so the wire format is `array(2) [roots, omap]`. Confirmed against
    // `Cardano.Ledger.Conway.Governance.Proposals.hs`:
    //   instance EraPParams era => EncCBOR (Proposals era) where
    //     encCBOR ps =
    //       let roots = toPrevGovActionIds $ ps ^. pRootsL
    //        in encCBOR (roots, ps ^. pPropsL)
    let outer_len = r.read_array_header()?;
    if outer_len != Some(2) {
        return Err(SerializationError::CborDecode(format!(
            "Proposals: expected outer array(2) [roots, omap], got {outer_len:?}"
        )));
    }

    // ── Element [0]: roots — `GovRelation StrictMaybe` = `array(4)` ─────
    //
    // `Cardano.Ledger.Conway.Governance.Procedures.hs`:
    //   encCBOR govPurpose@(GovRelation _ _ _ _) =
    //     let GovRelation {..} = govPurpose
    //      in encodeListLen 4
    //           <> encCBOR grPParamUpdate    -- [0]
    //           <> encCBOR grHardFork        -- [1]
    //           <> encCBOR grCommittee       -- [2]
    //           <> encCBOR grConstitution    -- [3]
    //
    // Each element is `StrictMaybe (GovPurposeId p)`; `GovPurposeId` is a
    // newtype over `GovActionId` with a derived-newtype `EncCBOR`, so it is
    // on the wire exactly as a bare `GovActionId` (`array(2) [hash32, uint]`).
    // `StrictMaybe` encodes as `array(0)` for `SNothing` and `array(1) [x]`
    // for `SJust x`.
    let roots_len = r.read_array_header()?;
    if roots_len != Some(4) {
        return Err(SerializationError::CborDecode(format!(
            "Proposals.roots: expected array(4), got {roots_len:?}"
        )));
    }
    let pparam_update = decode_strict_maybe_gov_action_id(&mut r, "grPParamUpdate")?;
    let hard_fork = decode_strict_maybe_gov_action_id(&mut r, "grHardFork")?;
    let committee = decode_strict_maybe_gov_action_id(&mut r, "grCommittee")?;
    let constitution = decode_strict_maybe_gov_action_id(&mut r, "grConstitution")?;
    let roots = HaskellGovRoots {
        pparam_update,
        hard_fork,
        committee,
        constitution,
    };

    // ── Element [1]: omap — `StrictSeq<GovActionState>` ─────────────────
    //
    // Haskell's `variableListLen` emits a definite-length array if N ≤ 23
    // and an indefinite-length array (`0x9f … 0xff`) otherwise; the
    // `for_each_array_item` helper transparently handles both.
    let mut actions: Vec<HaskellGovActionState> = Vec::new();
    let mut count = 0usize;
    r.for_each_array_item(|r| {
        if count >= MAX_PROPOSALS {
            return Err(SerializationError::CborDecode(format!(
                "Proposals: more than {MAX_PROPOSALS} entries (allocation bomb?)"
            )));
        }
        actions.push(decode_gov_action_state(r)?);
        count += 1;
        Ok(())
    })?;
    Ok(HaskellProposals { roots, actions })
}

/// Decode a `StrictMaybe (GovPurposeId p)`: `array(0)` = `SNothing`,
/// `array(1) [GovActionId]` = `SJust`.
fn decode_strict_maybe_gov_action_id(
    r: &mut crate::decode::reader::Reader<'_>,
    field: &'static str,
) -> Result<Option<HaskellGovActionId>, SerializationError> {
    match r.read_array_header()? {
        Some(0) => Ok(None),
        Some(1) => Ok(Some(decode_gov_action_id(r)?)),
        other => Err(SerializationError::CborDecode(format!(
            "Proposals.roots.{field}: StrictMaybe expected array(0) or array(1), got {other:?}"
        ))),
    }
}

fn decode_gov_action_state(
    r: &mut crate::decode::reader::Reader<'_>,
) -> Result<HaskellGovActionState, SerializationError> {
    let arr_len = r.read_array_header()?;
    if arr_len != Some(7) {
        return Err(SerializationError::CborDecode(format!(
            "GovActionState: expected array(7), got {arr_len:?}"
        )));
    }

    // [0] gasId — array(2) [tx_hash(32), index(uint)]
    let gas_id = decode_gov_action_id(r)?;

    // [1] gasCommitteeVotes — Map (Credential 'HotCommitteeRole) Vote
    let committee_votes = decode_credential_vote_map(r)?;

    // [2] gasDRepVotes — Map (Credential 'DRepRole) Vote
    let drep_votes = decode_credential_vote_map(r)?;

    // [3] gasStakePoolVotes — Map (KeyHash 'StakePool) Vote
    let pool_votes = decode_pool_vote_map(r)?;

    // [4] gasProposalProcedure — array(4) [deposit, return_addr, gov_action, anchor]
    // Mirrors the tx-body encoding; reuse the existing decoder. This module
    // decodes `ConwayGovState` specifically (see the module doc comment), so
    // the embedded `protocol_param_update`'s valid key set (issue #1013) is
    // Conway's (0-11, 16-33) — a Haskell-dumped fixture legitimately using a
    // Dijkstra-only key (34+) here would mean the fixture is not actually a
    // ConwayGovState, which should surface as a decode error, not be silently
    // widened to accept it.
    let procedure =
        crate::decode::era_conway::read_proposal_procedure(r, dugite_primitives::Era::Conway)?;

    // [5] gasProposedIn — uint
    let proposed_in = EpochNo(r.read_uint()?);

    // [6] gasExpiresAfter — uint
    let expires_after = EpochNo(r.read_uint()?);

    Ok(HaskellGovActionState {
        gas_id,
        committee_votes,
        drep_votes,
        pool_votes,
        procedure,
        proposed_in,
        expires_after,
    })
}

fn decode_gov_action_id(
    r: &mut crate::decode::reader::Reader<'_>,
) -> Result<HaskellGovActionId, SerializationError> {
    use dugite_primitives::hash::Hash32;
    let arr_len = r.read_array_header()?;
    if arr_len != Some(2) {
        return Err(SerializationError::CborDecode(format!(
            "GovActionId: expected array(2), got {arr_len:?}"
        )));
    }
    let bytes = r.read_bytes()?;
    if bytes.len() != 32 {
        return Err(SerializationError::CborDecode(format!(
            "GovActionId.tx_hash: expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    let mut h = [0u8; 32];
    h.copy_from_slice(bytes);
    let tx_hash = Hash32::from_bytes(h);
    let index = r.read_uint()?;
    Ok(HaskellGovActionId { tx_hash, index })
}

/// Result row from [`decode_credential_vote_map`]: `((credential_tag,
/// hash28), vote)` where `credential_tag` is `0` for key-credentials
/// and `1` for script-credentials. Lifted into a `type` alias to
/// silence clippy::type_complexity on the return type.
type CredentialVoteRow = ((u8, Hash28), HaskellVote);

fn decode_credential_vote_map(
    r: &mut crate::decode::reader::Reader<'_>,
) -> Result<Vec<CredentialVoteRow>, SerializationError> {
    let map_len = r.read_map_header()?;
    let len = map_len.ok_or_else(|| {
        SerializationError::CborDecode(
            "credential→vote map: expected definite-length, got indefinite".into(),
        )
    })?;
    let mut out = Vec::with_capacity(len as usize);
    for _ in 0..len {
        // Credential = array(2) [tag(0=key,1=script), hash28]
        let arr_len = r.read_array_header()?;
        if arr_len != Some(2) {
            return Err(SerializationError::CborDecode(format!(
                "credential: expected array(2), got {arr_len:?}"
            )));
        }
        let tag = r.read_uint()? as u8;
        let hash_bytes = r.read_bytes()?;
        if hash_bytes.len() != 28 {
            return Err(SerializationError::CborDecode(format!(
                "credential hash: expected 28 bytes, got {}",
                hash_bytes.len()
            )));
        }
        let mut h = [0u8; 28];
        h.copy_from_slice(hash_bytes);
        let cred = Hash28::from_bytes(h);
        let vote = decode_vote(r)?;
        out.push(((tag, cred), vote));
    }
    Ok(out)
}

fn decode_pool_vote_map(
    r: &mut crate::decode::reader::Reader<'_>,
) -> Result<Vec<(Hash28, HaskellVote)>, SerializationError> {
    let map_len = r.read_map_header()?;
    let len = map_len.ok_or_else(|| {
        SerializationError::CborDecode(
            "pool→vote map: expected definite-length, got indefinite".into(),
        )
    })?;
    let mut out = Vec::with_capacity(len as usize);
    for _ in 0..len {
        // KeyHash 'StakePool = bytes(28) — direct bytestring, no wrapper.
        let hash_bytes = r.read_bytes()?;
        if hash_bytes.len() != 28 {
            return Err(SerializationError::CborDecode(format!(
                "pool key hash: expected 28 bytes, got {}",
                hash_bytes.len()
            )));
        }
        let mut h = [0u8; 28];
        h.copy_from_slice(hash_bytes);
        let pool_id = Hash28::from_bytes(h);
        let vote = decode_vote(r)?;
        out.push((pool_id, vote));
    }
    Ok(out)
}

fn decode_vote(
    r: &mut crate::decode::reader::Reader<'_>,
) -> Result<HaskellVote, SerializationError> {
    let v = r.read_uint()?;
    match v {
        0 => Ok(HaskellVote::No),
        1 => Ok(HaskellVote::Yes),
        2 => Ok(HaskellVote::Abstain),
        other => Err(SerializationError::CborDecode(format!(
            "vote: expected 0/1/2, got {other}"
        ))),
    }
}

pub fn decode_committee(data: &[u8]) -> Result<(HaskellCommittee, usize), SerializationError> {
    let mut off = 0;

    // outer array(2)
    let (arr_len, n) = decode_array_len(&data[off..])?;
    off += n;
    if arr_len != 2 {
        return Err(SerializationError::CborDecode(format!(
            "Committee: expected array(2), got array({arr_len})"
        )));
    }

    // [0] committeeMembers — definite-length map
    let (map_len, n) = decode_map_len(&data[off..])?;
    off += n;
    let map_len = map_len.ok_or_else(|| {
        SerializationError::CborDecode(
            "Committee.members: expected definite-length map, got indefinite".into(),
        )
    })?;

    // #554: cap declared length to prevent allocation bomb via tampered gov-state.
    let members_cap = bounded_alloc_capacity(
        map_len,
        MAX_COMMITTEE_MEMBERS,
        data.len().saturating_sub(off),
    )?;
    let mut members: Vec<CommitteeMember> = Vec::with_capacity(members_cap);
    for _ in 0..map_len {
        let (cred, n) = decode_credential(&data[off..])?;
        off += n;
        let (expiry, n) = decode_uint(&data[off..])?;
        off += n;
        members.push((cred, expiry));
    }

    // [1] committeeThreshold — tag(30) array(2) [uint, uint].  `decode_rational`
    // accepts both tagged and untagged forms; Conway's `Committee` always tags
    // it but we tolerate either for forward-compat.
    let (threshold, n) = decode_rational(&data[off..])?;
    off += n;

    Ok((HaskellCommittee { members, threshold }, off))
}

#[cfg(test)]
mod proposals_roots_tests {
    use super::*;

    /// Build `array(1) [ array(2) [ bytes(32), uint ] ]` — a `SJust GovPurposeId`.
    fn sjust(fill: u8, index: u8) -> Vec<u8> {
        let mut v = vec![0x81, 0x82, 0x58, 0x20];
        v.extend_from_slice(&[fill; 32]);
        v.push(index); // small uint, 0..=23
        v
    }

    /// `array(0)` — `SNothing`.
    const SNOTHING: [u8; 1] = [0x80];

    /// `Proposals = array(2) [ GovRelation StrictMaybe (array(4)), omap ]`.
    fn proposals_cbor(roots: [Option<(u8, u8)>; 4], omap: &[u8]) -> Vec<u8> {
        let mut v = vec![0x82, 0x84];
        for r in roots {
            match r {
                None => v.extend_from_slice(&SNOTHING),
                Some((fill, idx)) => v.extend_from_slice(&sjust(fill, idx)),
            }
        }
        v.extend_from_slice(omap);
        v
    }

    /// #898 — the four `GovRelation` roots must land in the right fields.
    ///
    /// Canonical order (`Cardano.Ledger.Conway.Governance.Procedures.hs`):
    /// ```haskell
    /// encCBOR govPurpose@(GovRelation _ _ _ _) =
    ///   let GovRelation {..} = govPurpose
    ///    in encodeListLen 4
    ///         <> encCBOR grPParamUpdate    -- [0]
    ///         <> encCBOR grHardFork        -- [1]
    ///         <> encCBOR grCommittee       -- [2]
    ///         <> encCBOR grConstitution    -- [3]
    /// ```
    ///
    /// Every root gets a distinct fill byte and index so a transposition of any
    /// two positions fails this test. A wrong order is strictly worse than the
    /// pre-fix `None`: it would make the GOV rule accept proposals chaining onto
    /// the wrong purpose's root.
    #[test]
    fn govrelation_roots_decode_in_canonical_field_order() {
        let cbor = proposals_cbor(
            [
                Some((0x11, 1)), // grPParamUpdate
                Some((0x22, 2)), // grHardFork
                Some((0x33, 3)), // grCommittee
                Some((0x44, 4)), // grConstitution
            ],
            &[0x80], // empty omap
        );
        let p = decode_proposals_with_roots(&cbor).expect("decode");
        assert!(p.actions.is_empty(), "omap was empty");

        let got = |o: &Option<HaskellGovActionId>| {
            o.as_ref()
                .map(|i| (i.tx_hash.as_bytes()[0], i.index))
                .expect("root must be present")
        };
        assert_eq!(
            got(&p.roots.pparam_update),
            (0x11, 1),
            "grPParamUpdate is [0]"
        );
        assert_eq!(got(&p.roots.hard_fork), (0x22, 2), "grHardFork is [1]");
        assert_eq!(got(&p.roots.committee), (0x33, 3), "grCommittee is [2]");
        assert_eq!(
            got(&p.roots.constitution),
            (0x44, 4),
            "grConstitution is [3]"
        );
    }

    /// `SNothing` (`array(0)`) decodes to `None`, and mixes correctly with
    /// `SJust` siblings — the real preview snapshot has exactly this shape
    /// (a committee root present, others absent or present independently).
    #[test]
    fn govrelation_roots_handle_snothing_per_position() {
        let cbor = proposals_cbor([None, None, Some((0xab, 7)), None], &[0x80]);
        let p = decode_proposals_with_roots(&cbor).expect("decode");
        assert_eq!(p.roots.pparam_update, None);
        assert_eq!(p.roots.hard_fork, None);
        assert_eq!(
            p.roots
                .committee
                .as_ref()
                .map(|i| (i.tx_hash.as_bytes()[0], i.index)),
            Some((0xab, 7)),
            "only grCommittee is SJust"
        );
        assert_eq!(p.roots.constitution, None);
    }

    /// A `StrictMaybe` may only be `array(0)` or `array(1)`; anything else is a
    /// malformed snapshot and must be rejected rather than silently skipped.
    #[test]
    fn govrelation_root_rejects_malformed_strict_maybe() {
        // array(2) where a StrictMaybe belongs.
        let mut cbor = vec![0x82, 0x84, 0x82, 0x00, 0x00];
        cbor.extend_from_slice(&SNOTHING);
        cbor.extend_from_slice(&SNOTHING);
        cbor.extend_from_slice(&SNOTHING);
        cbor.push(0x80);
        let err = decode_proposals_with_roots(&cbor).expect_err("must reject array(2)");
        let msg = err.to_string();
        assert!(
            msg.contains("grPParamUpdate") && msg.contains("StrictMaybe"),
            "error must name the offending field: {msg}"
        );
    }

    /// All-`SNothing` roots with an empty omap is the legitimate shape for a
    /// chain that has never enacted anything (e.g. a fresh devnet).
    #[test]
    fn all_snothing_roots_decode_to_an_empty_proposals_set() {
        let p = decode_proposals_with_roots(&proposals_cbor([None; 4], &[0x80])).expect("decode");
        assert!(p.actions.is_empty());
        assert_eq!(p.roots, HaskellGovRoots::default());
    }
}
