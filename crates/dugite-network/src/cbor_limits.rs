//! Allocation caps for peer-driven CBOR decoders.
//!
//! Systemic security pattern #5 from the 2026-05-19 audit (#548): a malicious
//! peer can declare an absurd definite-length header (e.g. `array(u64::MAX)`)
//! and force `Vec::with_capacity(N)` / `Vec::reserve(N)` to attempt a huge
//! allocation before the decode loop ever runs. Even if the allocation fails
//! gracefully via the system allocator, the attempt itself triggers an
//! `abort` in many configurations or wedges the process.
//!
//! **Defense pattern (use everywhere a peer-supplied length feeds an
//! allocation):**
//!
//! 1. Decode the length header.
//! 2. Compare it to the protocol-spec maximum BEFORE allocating.
//! 3. Compare it to `remaining_bytes / minimum_element_size` BEFORE allocating
//!    — a peer can claim a million elements but only have 10 bytes of input
//!    left, so the declared length must be physically realisable.
//! 4. Pre-allocate at the capped value, then run the decode loop with the
//!    real check on each element.
//!
//! The helpers in this module enforce step 2+3 and produce a safe capacity for
//! step 4. The decode loop itself still has to validate each element.
//!
//! **Reference fixes:**
//! - `MAX_INTERSECT_POINTS` (B19, ChainSync intersect) — `mod.rs`
//! - `MAX_INFLIGHT` (B2, TxSubmission2) — `txsubmission/mod.rs`
//! - `MAX_SHARED_ADDRS` (PeerSharing) — `peersharing/mod.rs`
//! - `MAX_HANDSHAKE_VERSIONS` (A-003, Handshake) — `handshake/mod.rs`
//!
//! This module exists so that NEW decoders can reach for `bounded_capacity`
//! by default instead of `with_capacity(declared_len as usize)`.

/// Maximum number of entries in any tx metadata map/array.
///
/// Mirrors Haskell's tolerance: `protocolParamMaxTxSize` (16 KiB on mainnet)
/// divided by the minimum metadatum entry size (1 byte for an empty int).
/// A real-world metadata map never exceeds a few hundred entries.
pub const MAX_METADATA_ENTRIES: u64 = 16_384;

/// Maximum nesting depth for tx metadata Map/List values.
///
/// Haskell's `MetadataValidationError` rejects deeply nested metadata to
/// prevent stack overflow during recursive decode. 64 mirrors Haskell.
pub const MAX_METADATA_DEPTH: u32 = 64;

/// Maximum number of multi-asset policy entries in a single tx output.
///
/// The CDDL spec doesn't enforce a hard cap but real-world outputs are
/// bounded by `protocolParamMaxValSize` (5000 bytes on mainnet). At 28 bytes
/// per policy ID + 1 byte per asset name + 1 byte per value, the practical
/// ceiling is ~150 policy entries per output.
pub const MAX_MULTIASSET_POLICIES: u64 = 1_024;

/// Maximum number of asset names under a single policy.
pub const MAX_ASSETS_PER_POLICY: u64 = 1_024;

/// Maximum number of witnesses of any kind in a single tx.
///
/// Haskell's `validateMaxTxSize` indirectly bounds this. Real-world txs
/// rarely exceed a few hundred witnesses; 16384 is generous.
pub const MAX_TX_WITNESSES: u64 = 16_384;

/// Generic outer cap for any peer-supplied CBOR length on the mini-protocol
/// envelope itself (not the embedded tx payload). Mux frames cap at 65535
/// bytes payload, so no envelope length can exceed that.
pub const MAX_ENVELOPE_ELEMENTS: u64 = 65_536;

/// Minimum number of CBOR bytes that ANY element occupies.
///
/// Every CBOR value — even `null` (0xf6), `true` (0xf5), `0` (0x00) — is at
/// least one byte. So if the peer declares N elements but the remaining input
/// has fewer than N bytes, the declared length is impossible.
pub const MIN_CBOR_ELEMENT_BYTES: usize = 1;

/// Compute a safe capacity to use for `Vec::with_capacity` / `reserve` based
/// on a peer-supplied length header.
///
/// Returns `Err` if the declared length exceeds the protocol-spec maximum
/// (`max_allowed`) OR is physically impossible given the remaining input.
///
/// # Arguments
/// * `declared_len` — the CBOR-decoded length header, as `u64` (CBOR maps and
///   arrays can declare up to `u64::MAX` elements).
/// * `max_allowed` — the protocol-spec cap; pass `u64::MAX` only if you have
///   another bound below.
/// * `remaining_bytes` — bytes left in the input buffer. Used as a physical
///   upper bound: `declared_len <= remaining_bytes / min_element_bytes`.
/// * `min_element_bytes` — the minimum number of bytes any single element of
///   the array/map can occupy. Pass `MIN_CBOR_ELEMENT_BYTES` (=1) for generic
///   CBOR values, or a larger value for fixed-shape elements (e.g. a 32-byte
///   hash + 1-byte index = 33).
///
/// # Returns
/// A `usize` capped at `min(declared_len, max_allowed, remaining_bytes /
/// min_element_bytes, isize::MAX as usize)`. Never returns a value that
/// `Vec::with_capacity` will refuse.
///
/// # Errors
/// Returns `Err(BoundedCapacityError)` describing why the declared length is
/// rejected. Callers should propagate this as a decode error.
pub fn bounded_capacity(
    declared_len: u64,
    max_allowed: u64,
    remaining_bytes: usize,
    min_element_bytes: usize,
) -> Result<usize, BoundedCapacityError> {
    // Step 1: protocol-spec cap.
    if declared_len > max_allowed {
        return Err(BoundedCapacityError::ExceedsProtocolMax {
            declared: declared_len,
            max: max_allowed,
        });
    }

    // Step 2: physical-realisability cap.
    //
    // `min_element_bytes == 0` would be a programming error — CBOR has no
    // zero-byte values — so clamp to 1.
    let min_bytes = min_element_bytes.max(1);
    let physical_max = remaining_bytes / min_bytes;
    if declared_len as usize > physical_max {
        return Err(BoundedCapacityError::ExceedsRemainingInput {
            declared: declared_len,
            remaining_bytes,
            min_element_bytes: min_bytes,
        });
    }

    // Step 3: clamp to platform max (isize::MAX is the alloc-API limit).
    let capped = (declared_len as usize)
        .min(physical_max)
        .min(usize::MAX / 2);
    Ok(capped)
}

/// Errors returned by `bounded_capacity`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BoundedCapacityError {
    /// The peer's declared length exceeds the protocol-spec maximum.
    ExceedsProtocolMax { declared: u64, max: u64 },
    /// The peer's declared length is physically impossible given the
    /// remaining input.
    ExceedsRemainingInput {
        declared: u64,
        remaining_bytes: usize,
        min_element_bytes: usize,
    },
}

impl std::fmt::Display for BoundedCapacityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ExceedsProtocolMax { declared, max } => {
                write!(
                    f,
                    "declared CBOR length {declared} exceeds protocol maximum {max}"
                )
            }
            Self::ExceedsRemainingInput {
                declared,
                remaining_bytes,
                min_element_bytes,
            } => {
                write!(
                    f,
                    "declared CBOR length {declared} exceeds physical maximum \
                     ({remaining_bytes} bytes / {min_element_bytes} bytes-per-element \
                     = {} elements)",
                    remaining_bytes / min_element_bytes.max(&1)
                )
            }
        }
    }
}

impl std::error::Error for BoundedCapacityError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_capacity_happy_path() {
        // 100 elements, 1 KiB remaining, 1-byte min size — comfortable.
        let cap = bounded_capacity(100, 1000, 1024, 1).unwrap();
        assert_eq!(cap, 100);
    }

    #[test]
    fn bounded_capacity_rejects_oversized_declared() {
        let err = bounded_capacity(u64::MAX, 100, 1024, 1).unwrap_err();
        assert!(matches!(
            err,
            BoundedCapacityError::ExceedsProtocolMax {
                declared: u64::MAX,
                max: 100
            }
        ));
    }

    #[test]
    fn bounded_capacity_rejects_oversized_max_u32() {
        // The historically-reported attack: declared = u32::MAX.
        let err = bounded_capacity(u32::MAX as u64, 100, 1024, 1).unwrap_err();
        assert!(matches!(
            err,
            BoundedCapacityError::ExceedsProtocolMax { .. }
        ));
    }

    #[test]
    fn bounded_capacity_rejects_physically_impossible() {
        // Declared 1000 elements but only 10 bytes of input.
        let err = bounded_capacity(1000, 10_000, 10, 1).unwrap_err();
        assert!(matches!(
            err,
            BoundedCapacityError::ExceedsRemainingInput {
                declared: 1000,
                remaining_bytes: 10,
                ..
            }
        ));
    }

    #[test]
    fn bounded_capacity_rejects_when_element_min_size_large() {
        // 1000 elements declared, 32-byte min element, but only 100 bytes
        // remaining = max 3 elements physically.
        let err = bounded_capacity(1000, 100_000, 100, 32).unwrap_err();
        assert!(matches!(
            err,
            BoundedCapacityError::ExceedsRemainingInput { .. }
        ));
    }

    #[test]
    fn bounded_capacity_clamps_to_protocol_max() {
        // declared exactly equals max — allowed.
        let cap = bounded_capacity(100, 100, 10_000, 1).unwrap();
        assert_eq!(cap, 100);
    }

    #[test]
    fn bounded_capacity_zero_remaining() {
        let err = bounded_capacity(1, 1000, 0, 1).unwrap_err();
        assert!(matches!(
            err,
            BoundedCapacityError::ExceedsRemainingInput { .. }
        ));
    }

    #[test]
    fn bounded_capacity_zero_declared() {
        // Zero declared length is fine.
        let cap = bounded_capacity(0, 100, 1024, 1).unwrap();
        assert_eq!(cap, 0);
    }

    #[test]
    fn bounded_capacity_min_element_zero_treated_as_one() {
        // Defensive: if a caller passes 0 for min_element_bytes, we don't
        // divide by zero.
        let cap = bounded_capacity(10, 1000, 100, 0).unwrap();
        assert_eq!(cap, 10);
    }

    #[test]
    fn bounded_capacity_error_messages_human_readable() {
        let err = BoundedCapacityError::ExceedsProtocolMax {
            declared: u64::MAX,
            max: 100,
        };
        let msg = format!("{err}");
        assert!(msg.contains("exceeds protocol maximum"));
        assert!(msg.contains("100"));

        let err = BoundedCapacityError::ExceedsRemainingInput {
            declared: 1000,
            remaining_bytes: 10,
            min_element_bytes: 1,
        };
        let msg = format!("{err}");
        assert!(msg.contains("exceeds physical maximum"));
    }
}
