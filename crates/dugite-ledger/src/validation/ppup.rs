//! PPUP (pre-Conway protocol-parameter update) validation rules.
//!
//! Reference: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`.
//!
//! PPUP is active in Shelley–Babbage (`AtMostEra "Babbage" era`).  Conway
//! replaces this rule with on-chain governance (CIP-1694).  All three
//! PPUP error predicates short-circuit (no-op) at PV >= 9.
//!
//! | Predicate                | Check                                            |
//! |--------------------------|--------------------------------------------------|
//! | `NonGenesisUpdatePPUP`   | proposed update keys ⊆ genesis-delegate keys    |
//! | `PPUpdateWrongEpoch`     | target epoch == current (this period) or +1 (next) |
//! | `PVCannotFollowPPUP`     | new ProtVer is `(maj, min+1)` or `(maj+1, 0)`   |
//!
//! ## Voting period
//!
//! `tooLate = firstSlotOfNextEpoch - 2 * stabilityWindow`
//!
//! - When `currentSlot < tooLate`  → [`VotingPeriod::ForThisEpoch`].
//! - When `currentSlot >= tooLate` → [`VotingPeriod::ForNextEpoch`].
//!
//! Where `stabilityWindow = ceil(3 * k / f)` (`k = securityParam`,
//! `f = activeSlotCoeff`).
//!
//! ## Quorum / enactment
//!
//! [`voted_future_pparams`] is a non-error helper that decides whether a
//! quorum of genesis delegates has voted for the same `PParamsUpdate`
//! value.  When exactly one update has at least `quorum` votes, that
//! update is returned (subject to a structural sanity check
//! `max_tx_size + max_block_header_size < max_block_body_size`).  This
//! mirrors Haskell's `votedFuturePParams` and is silent on disagreement —
//! the function returns `None` for "no quorum", "tied", or "fails sanity",
//! never an error.

use std::collections::{BTreeMap, HashMap};

use dugite_primitives::hash::{Hash28, Hash32};
use dugite_primitives::protocol_params::ProtocolParameters;
use dugite_primitives::transaction::{ProtocolParamUpdate, UpdateProposal};

use super::{ValidationContext, ValidationError, VotingPeriod};

/// Validate a pre-Conway protocol-parameter update proposal against the
/// supplied context.
///
/// `params` and `current_slot` are passed in directly (matching the rest
/// of the validation surface), while genesis-delegate set, epoch
/// geometry, and current epoch come from `ValidationContext`.
///
/// Returns `Ok(())` when:
/// - the proposal is `None` (no-op);
/// - the protocol version is Conway+ (`>= 9`) — Conway has no PPUP;
/// - all 3 predicates pass (or are silently skipped because their
///   context is unavailable).
///
/// Returns `Err(errors)` aggregating every predicate failure, mirroring
/// Haskell's `NonEmpty` predicate-failure shape.
pub fn validate_ppup(
    update: Option<&UpdateProposal>,
    params: &ProtocolParameters,
    current_slot: u64,
    ctx: &ValidationContext,
) -> Result<(), Vec<ValidationError>> {
    // Conway and onward: PPUP is replaced by on-chain governance.  No-op.
    if params.protocol_version_major >= 9 {
        return Ok(());
    }
    let Some(update) = update else {
        return Ok(());
    };

    let mut errors = Vec::new();
    check_non_genesis_update(update, ctx, &mut errors);
    check_pp_update_epoch(update, params, current_slot, ctx, &mut errors);
    check_pv_can_follow(update, params, &mut errors);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------
// `NonGenesisUpdatePPUP`
// ---------------------------------------------------------------------------

/// Reject update proposals whose key set is not a subset of the
/// registered genesis-delegate keys.
///
/// Reference: Haskell `NonGenesisUpdatePPUP` in
/// `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ppup.hs`:
/// ```text
/// pup_keys ⊆ dom (genDelegs ds) ?! NonGenesisUpdatePPUP …
/// ```
///
/// Silently skipped (lenient default) when
/// `ValidationContext::genesis_delegates` is `None`.
///
/// On the dugite wire, `UpdateProposal.proposed_updates` keys are
/// `Hash32` (zero-padded from the on-chain 28-byte genesis-key hash).
/// We compare on the leading 28 bytes against `Hash28`-keyed
/// `genesis_delegates`.
pub(crate) fn check_non_genesis_update(
    update: &UpdateProposal,
    ctx: &ValidationContext,
    errors: &mut Vec<ValidationError>,
) {
    let Some(genesis_keys) = ctx.genesis_delegates.as_ref() else {
        return;
    };

    let mut bad: Vec<String> = Vec::new();
    for (key32, _ppu) in &update.proposed_updates {
        let key28 = hash32_to_hash28(key32);
        if !genesis_keys.contains(&key28) {
            bad.push(key28.to_hex());
        }
    }

    if !bad.is_empty() {
        bad.sort(); // deterministic for diagnostic stability
        let mut genesis_hex: Vec<String> = genesis_keys.iter().map(|h| h.to_hex()).collect();
        genesis_hex.sort();
        errors.push(ValidationError::NonGenesisUpdatePPUP {
            proposed: bad,
            genesis: genesis_hex,
        });
    }
}

// ---------------------------------------------------------------------------
// `PPUpdateWrongEpoch`
// ---------------------------------------------------------------------------

/// Reject update proposals whose declared `epoch` does not match the
/// current voting period:
///
/// - `current_slot < tooLate` → target must equal `current_epoch`
///   ([`VotingPeriod::ForThisEpoch`]).
/// - `current_slot >= tooLate` → target must equal `current_epoch + 1`
///   ([`VotingPeriod::ForNextEpoch`]).
///
/// Where `tooLate = firstSlotOfNextEpoch - 2 * stabilityWindow`.
///
/// Reference: Haskell `PPUpdateWrongEpoch` /
/// `votingPeriod` in `Shelley.Rules.Ppup`.
///
/// Silently skipped when `epoch_length` or `security_param` are not
/// supplied on the context — the predicate cannot fire without them.
pub(crate) fn check_pp_update_epoch(
    update: &UpdateProposal,
    params: &ProtocolParameters,
    current_slot: u64,
    ctx: &ValidationContext,
    errors: &mut Vec<ValidationError>,
) {
    let Some(epoch_length) = ctx.epoch_length else {
        return;
    };
    // PPUP uses `2 * stabilityWindow` (Haskell `tooLate = ...
    // - (2 * stabilityWindow)`) — this is the only structural
    // difference from the MIR `checkSlotNotTooLate` predicate.
    let Some(stability_window) = super::mir::compute_stability_window(params, ctx.security_param)
    else {
        return;
    };

    let current_epoch = ctx
        .current_epoch
        .unwrap_or_else(|| current_slot / epoch_length);
    let first_slot_next_epoch = current_epoch.saturating_add(1).saturating_mul(epoch_length);
    let too_late = first_slot_next_epoch.saturating_sub(stability_window.saturating_mul(2));

    let (period, expected) = if current_slot < too_late {
        (VotingPeriod::ForThisEpoch, current_epoch)
    } else {
        (VotingPeriod::ForNextEpoch, current_epoch.saturating_add(1))
    };

    if update.epoch != expected {
        errors.push(ValidationError::PPUpdateWrongEpoch {
            current: current_epoch,
            target: update.epoch,
            period,
        });
    }
}

// ---------------------------------------------------------------------------
// `PVCannotFollowPPUP`
// ---------------------------------------------------------------------------

/// Reject update proposals whose proposed protocol version is not a
/// valid successor to the current one.
///
/// Allowed transitions from `(maj, min)`:
/// - `(maj, min + 1)` — minor bump.
/// - `(maj + 1, 0)` — major bump (resets minor).
///
/// Anything else (regression, skipping versions, etc.) is rejected.
///
/// Reference: Haskell `PVCannotFollowPPUP` /
/// `pvCanFollow` in `Shelley.Rules.Ppup`.
pub(crate) fn check_pv_can_follow(
    update: &UpdateProposal,
    params: &ProtocolParameters,
    errors: &mut Vec<ValidationError>,
) {
    let current_major = params.protocol_version_major as u32;
    let current_minor = params.protocol_version_minor as u32;

    for (_, ppu) in &update.proposed_updates {
        // Either both fields are present (a real PV proposal) or both
        // absent (the PPU does not touch protocol version at all).
        // Haskell's `pvCanFollow` only fires when a new PV is proposed.
        let (Some(new_major), Some(new_minor)) =
            (ppu.protocol_version_major, ppu.protocol_version_minor)
        else {
            continue;
        };
        let new_major = new_major as u32;
        let new_minor = new_minor as u32;

        let minor_bump = new_major == current_major && new_minor == current_minor + 1;
        let major_bump = new_major == current_major + 1 && new_minor == 0;
        if !(minor_bump || major_bump) {
            errors.push(ValidationError::PVCannotFollowPPUP {
                bad_pv: (new_major, new_minor),
            });
        }
    }
}

// ---------------------------------------------------------------------------
// Quorum / enactment helper (`voted_future_pparams`)
// ---------------------------------------------------------------------------

/// Fold a raw `(genesis-key, PParamsUpdate)` proposal list into a
/// per-genesis-key map, matching Haskell's `Map (KeyHash 'Genesis)
/// (PParamsUpdate era)` semantics: a later proposal from the same genesis
/// delegate overwrites (never merges with) an earlier one from the same
/// key. Truncates the wire's zero-padded `Hash32` genesis keys down to
/// their on-chain `Hash28` form (mirrors [`check_non_genesis_update`]).
///
/// Callers must supply `proposals` in submission order (oldest first) so
/// that `BTreeMap::insert`'s last-writer-wins behaviour reproduces
/// Haskell's `Map.insert` overwrite-on-repeated-key semantics.
///
/// Feeds [`voted_future_pparams`] from the three enactment sites
/// (`eras::shelley`, `eras::conway`, `state::epoch`) and the three
/// header/envelope forecast helpers in `state::mod` (issue #784).
pub fn fold_pp_proposals(
    proposals: &[(Hash32, ProtocolParamUpdate)],
) -> BTreeMap<Hash28, ProtocolParamUpdate> {
    let mut map = BTreeMap::new();
    for (genesis_hash, ppu) in proposals {
        map.insert(hash32_to_hash28(genesis_hash), ppu.clone());
    }
    map
}

/// Tally votes among proposed protocol-parameter updates and return the
/// single update that has reached quorum, if any.
///
/// Mirrors Haskell `votedFuturePParams` in `Shelley.Rules.Ppup`:
/// ```text
/// votedValue ≡ a single Update value that >= quorum genesis delegates
///              voted for; if no value reaches quorum (or there's a tie
///              of two distinct values both at quorum), return Nothing.
/// ```
///
/// After tallying, the merged update must satisfy the structural sanity
/// check
/// `max_tx_size + max_block_header_size < max_block_body_size`
/// (Haskell `applyPPUpdates`).  Failures are silently discarded —
/// `voted_future_pparams` is not an error path.
///
/// `current` is the currently-active `ProtocolParameters`; only the
/// three size fields are read.
pub fn voted_future_pparams(
    proposed: &BTreeMap<Hash28, ProtocolParamUpdate>,
    quorum: u64,
    current: &ProtocolParameters,
) -> Option<ProtocolParamUpdate> {
    if proposed.is_empty() || quorum == 0 {
        return None;
    }

    // Tally identical updates by structural equality.  We use a Vec of
    // `(ppu, count)` rather than a HashMap because `ProtocolParamUpdate`
    // is not `Hash`; the input is genesis-delegate-bounded (≤ ~10 keys
    // in practice on mainnet) so O(n²) is acceptable.
    let mut tally: Vec<(ProtocolParamUpdate, u64)> = Vec::new();
    for ppu in proposed.values() {
        if let Some(slot) = tally.iter_mut().find(|(p, _)| p == ppu) {
            slot.1 += 1;
        } else {
            tally.push((ppu.clone(), 1));
        }
    }

    // Keep only updates that meet the quorum.  If exactly one update
    // qualifies we return it; on tie or no-quorum return None.
    let mut at_quorum: Vec<&ProtocolParamUpdate> = tally
        .iter()
        .filter(|(_, count)| *count >= quorum)
        .map(|(p, _)| p)
        .collect();
    if at_quorum.len() != 1 {
        return None;
    }
    let winner = at_quorum.remove(0);

    // Structural sanity: `max_tx_size + max_block_header_size <
    // max_block_body_size`.  Take the proposed value when set,
    // otherwise the current value.
    let max_tx = winner.max_tx_size.unwrap_or(current.max_tx_size);
    let max_hdr = winner
        .max_block_header_size
        .unwrap_or(current.max_block_header_size);
    let max_body = winner
        .max_block_body_size
        .unwrap_or(current.max_block_body_size);
    if max_tx.saturating_add(max_hdr) >= max_body {
        return None;
    }

    Some(winner.clone())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Truncate a `Hash32` to its leading 28 bytes (the on-chain encoding
/// for genesis-key / pool-key hashes that have been padded into the
/// `Hash32` slot).
fn hash32_to_hash28(h: &Hash32) -> Hash28 {
    let mut out = [0u8; 28];
    out.copy_from_slice(&h.as_bytes()[..28]);
    Hash28::from_bytes(out)
}

// Type alias for symmetry with Haskell's
// `Map (KeyHash 'Genesis) (PParamsUpdate era)` (used by callers that
// build a tally directly).
pub type ProposedUpdates = HashMap<Hash28, ProtocolParamUpdate>;

#[cfg(test)]
mod tests;
