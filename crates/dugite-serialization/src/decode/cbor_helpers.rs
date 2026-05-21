//! Pallas-free byte-level helpers for walking outer Cardano block CBOR.
//!
//! These helpers measure / extract pieces of a Cardano block envelope without
//! decoding the full block. They were previously hosted in `crate::multi_era`
//! alongside the pallas-backed decoder; M6 cutover moves them here as
//! `crate::multi_era` is deleted.
//!
//! Used by:
//! - [`crate::extract_block_identity`] (mithril import) for slot/block-no probes
//! - Storage layer for body-size checks before disk write
//! - The BBODY ledger rule (`validate_block_body_hash`) for per-component bytes
//!
//! All helpers delegate to the byte-exact
//! [`crate::haskell_snapshot::cbor_utils::skip_cbor_value`] primitive, which has
//! been validated against the Haskell cardano-node CBOR encoder.

/// Compute the actual block body size from raw block CBOR bytes.
///
/// Cardano Shelley+ blocks are serialized as `(era_tag, [header, tx_bodies,
/// witness_sets, aux_data_map, invalid_txs])`.  The header's `block_body_size`
/// field records the total serialized byte count of the 4 body components
/// (indices 1..4 of the inner array).
///
/// This function parses the CBOR structure to measure those 4 components from
/// the original wire bytes, giving a byte-exact value that can be compared
/// against the header claim for the BBODY ledger rule.
///
/// Returns `None` for Byron/EBB blocks (which have no body_size header field)
/// or if the CBOR structure is unexpected.
pub fn compute_block_body_size_from_cbor(raw_cbor: &[u8]) -> Option<u64> {
    use crate::haskell_snapshot::cbor_utils::skip_cbor_value;

    // The wire format is a 2-element CBOR array: [era_tag, inner_block].
    // Byron blocks use era_tag 0 (main) or 1 (EBB); Shelley+ use 2..6.
    if raw_cbor.is_empty() {
        return None;
    }

    // Parse outer array header (should be a 2-element array: 0x82)
    let outer_major = raw_cbor[0] >> 5;
    if outer_major != 4 {
        return None; // not an array
    }

    // Skip the array header
    let info = raw_cbor[0] & 0x1f;
    let (outer_len, mut off) = match info {
        0..=23 => (info as u64, 1usize),
        24 if raw_cbor.len() >= 2 => (raw_cbor[1] as u64, 2),
        _ => return None,
    };
    if outer_len != 2 {
        return None; // unexpected outer structure
    }

    // Skip the era tag (first element: a uint)
    let era_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
    let era_major = raw_cbor[off] >> 5;
    if era_major != 0 {
        return None; // not a uint era tag
    }
    // Read era tag value to filter out Byron
    let era_info = raw_cbor[off] & 0x1f;
    let era_tag = match era_info {
        0..=23 => era_info as u64,
        24 if raw_cbor.len() > off + 1 => raw_cbor[off + 1] as u64,
        _ => return None,
    };
    // Byron main blocks (tag 0) and EBBs (tag 1) don't have body_size
    if era_tag <= 1 {
        return None;
    }
    off += era_size;

    // Now at the inner block array: [header, tx_bodies, witnesses, aux_data, invalid_txs]
    // Parse the inner array header
    if off >= raw_cbor.len() {
        return None;
    }
    let inner_major = raw_cbor[off] >> 5;
    if inner_major != 4 {
        return None; // not an array
    }
    let inner_info = raw_cbor[off] & 0x1f;
    let (inner_len, hdr_bytes) = match inner_info {
        0..=23 => (inner_info as u64, 1usize),
        24 if raw_cbor.len() > off + 1 => (raw_cbor[off + 1] as u64, 2),
        _ => return None,
    };
    // Shelley+ blocks have 5 elements (some early eras may have 4)
    if inner_len < 4 {
        return None;
    }
    off += hdr_bytes;

    // Skip the header (index 0)
    let header_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
    off += header_size;

    // Measure body components (indices 1..inner_len-1)
    let body_start = off;
    let body_components = inner_len - 1; // everything except the header
    for _ in 0..body_components {
        let item_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
        off += item_size;
    }

    Some((off - body_start) as u64)
}

/// Extract the raw CBOR bytes of the block body from a full block CBOR buffer.
///
/// The Cardano wire format is `[era_tag, [header, tx_bodies, witnesses, aux_data,
/// invalid_txs]]`.  The block body hash (`header.body_hash`) is
/// `blake2b_256(body_bytes)` where `body_bytes` is the concatenation of the
/// serialized body components (indices 1..N-1 of the inner array, i.e.
/// everything except the header at index 0).
///
/// This matches Haskell's `mkOriginalBlockBodyHashed` in
/// `Ouroboros.Consensus.Shelley.Ledger.Block`.
///
/// Returns `None` for Byron/EBB blocks (which have no `body_hash` header field)
/// or if the CBOR structure is not parseable.
///
/// Issue #545 E5: used to wire body-hash verification into `apply_fetched_block`.
pub fn extract_block_body_cbor(raw_cbor: &[u8]) -> Option<&[u8]> {
    use crate::haskell_snapshot::cbor_utils::skip_cbor_value;

    if raw_cbor.is_empty() {
        return None;
    }

    // Outer array: [era_tag, inner_block]
    let outer_major = raw_cbor[0] >> 5;
    if outer_major != 4 {
        return None;
    }
    let info = raw_cbor[0] & 0x1f;
    let (outer_len, mut off) = match info {
        0..=23 => (info as u64, 1usize),
        24 if raw_cbor.len() >= 2 => (raw_cbor[1] as u64, 2),
        _ => return None,
    };
    if outer_len != 2 {
        return None;
    }

    // Read and skip era tag
    if off >= raw_cbor.len() {
        return None;
    }
    let era_major = raw_cbor[off] >> 5;
    if era_major != 0 {
        return None;
    }
    let era_info = raw_cbor[off] & 0x1f;
    let era_tag = match era_info {
        0..=23 => era_info as u64,
        24 if raw_cbor.len() > off + 1 => raw_cbor[off + 1] as u64,
        _ => return None,
    };
    if era_tag <= 1 {
        // Byron — no body_hash
        return None;
    }
    let era_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
    off += era_size;

    // Inner array: [header, tx_bodies, witnesses, aux_data, invalid_txs]
    if off >= raw_cbor.len() {
        return None;
    }
    let inner_major = raw_cbor[off] >> 5;
    if inner_major != 4 {
        return None;
    }
    let inner_info = raw_cbor[off] & 0x1f;
    let (inner_len, hdr_bytes) = match inner_info {
        0..=23 => (inner_info as u64, 1usize),
        24 if raw_cbor.len() > off + 1 => (raw_cbor[off + 1] as u64, 2),
        _ => return None,
    };
    if inner_len < 4 {
        return None;
    }
    off += hdr_bytes;

    // Skip the header (index 0)
    let header_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
    off += header_size;

    // Body starts here: indices 1..inner_len-1 (tx_bodies, witnesses, aux_data, invalid_txs)
    let body_start = off;
    let body_components = inner_len - 1;
    for _ in 0..body_components {
        let item_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
        off += item_size;
    }

    Some(&raw_cbor[body_start..off])
}

/// Extract the raw CBOR bytes of each individual block-body component.
///
/// The Cardano wire format is `[era_tag, [header, c_0, c_1, ..., c_{N-1}]]`.
/// Returns a `Vec` of slices, one per component, in the order they appear on
/// the wire. The exact component count depends on the era:
///
/// - Shelley/Allegra/Mary (`inner_len == 4`): 3 components — `tx_bodies`,
///   `tx_witness_sets`, `auxiliary_data_set`.
/// - Alonzo and later (`inner_len == 5`): 4 components — `tx_bodies`,
///   `tx_witness_sets`, `auxiliary_data_set`, `invalid_transactions`.
///
/// Each slice covers the byte range of one component, exactly as encoded in
/// `raw_cbor` (no copying, no re-serialization). These are the byte ranges
/// used as input to `bbHash` — see Haskell `Cardano.Ledger.Alonzo.BlockBody`
/// (`hashAlonzoSegWits` / `txSeqBodyBytes`).
///
/// Returns `None` for Byron/EBB blocks (no `body_hash` to verify) or any
/// CBOR structure that doesn't match the expected `[tag, [header, ...]]`
/// shape.
///
/// Issue #550 E5: per-component view used by `validate_block_body_hash` to
/// match the Haskell `bbHash` algorithm exactly.
pub fn extract_block_body_components(raw_cbor: &[u8]) -> Option<Vec<&[u8]>> {
    use crate::haskell_snapshot::cbor_utils::skip_cbor_value;

    if raw_cbor.is_empty() {
        return None;
    }

    // Outer array: [era_tag, inner_block]
    let outer_major = raw_cbor[0] >> 5;
    if outer_major != 4 {
        return None;
    }
    let info = raw_cbor[0] & 0x1f;
    let (outer_len, mut off) = match info {
        0..=23 => (info as u64, 1usize),
        24 if raw_cbor.len() >= 2 => (raw_cbor[1] as u64, 2),
        _ => return None,
    };
    if outer_len != 2 {
        return None;
    }

    // Read and skip era tag
    if off >= raw_cbor.len() {
        return None;
    }
    let era_major = raw_cbor[off] >> 5;
    if era_major != 0 {
        return None;
    }
    let era_info = raw_cbor[off] & 0x1f;
    let era_tag = match era_info {
        0..=23 => era_info as u64,
        24 if raw_cbor.len() > off + 1 => raw_cbor[off + 1] as u64,
        _ => return None,
    };
    if era_tag <= 1 {
        // Byron — no body_hash
        return None;
    }
    let era_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
    off += era_size;

    // Inner array: [header, c_0, c_1, ..., c_{N-1}]
    if off >= raw_cbor.len() {
        return None;
    }
    let inner_major = raw_cbor[off] >> 5;
    if inner_major != 4 {
        return None;
    }
    let inner_info = raw_cbor[off] & 0x1f;
    let (inner_len, hdr_bytes) = match inner_info {
        0..=23 => (inner_info as u64, 1usize),
        24 if raw_cbor.len() > off + 1 => (raw_cbor[off + 1] as u64, 2),
        _ => return None,
    };
    // Need at least 4 elements (header + 3 body components for Shelley/Mary)
    if inner_len < 4 {
        return None;
    }
    off += hdr_bytes;

    // Skip the header (index 0)
    let header_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
    off += header_size;

    // Capture each component slice
    let n_components = (inner_len - 1) as usize;
    let mut components = Vec::with_capacity(n_components);
    for _ in 0..n_components {
        let start = off;
        let item_size = skip_cbor_value(&raw_cbor[off..]).ok()?;
        off += item_size;
        components.push(&raw_cbor[start..off]);
    }

    Some(components)
}
