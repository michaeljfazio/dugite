//! Flat wire codec for UPLC programs.
//!
//! UPLC programs are wire-encoded in a custom bit-level format ("flat")
//! that predates the project's adoption of CBOR. The outer container
//! is a CBOR byte-string holding the flat-encoded program; CBOR is
//! handled in `crate::program`, while *this* module handles the inner
//! flat layer.
//!
//! Reference: `plutus-core/plutus-core/src/PlutusCore/Flat.hs` in
//! IntersectMBO/plutus is the normative implementation. The encoding
//! is described informally in the Plutus tech report appendix.
//!
//! ## Encoding shape (informal)
//!
//! Each `Term` constructor is identified by a 4-bit tag:
//!
//! | tag (binary) | constructor    |
//! |--------------|----------------|
//! | `0000`       | Var            |
//! | `0001`       | Delay          |
//! | `0010`       | Lam            |
//! | `0011`       | App            |
//! | `0100`       | Const          |
//! | `0101`       | Force          |
//! | `0110`       | Error          |
//! | `0111`       | Builtin        |
//! | `1000`       | Constr (V3+)   |
//! | `1001`       | Case (V3+)     |
//!
//! Constants are prefixed by a *universe-tag bit-list* that encodes
//! the type (recursively, for `list`/`pair`), followed by the payload.
//!
//! Variables encode their De Bruijn index as a `Natural` (chunked
//! 7-bit varint, high bit = continue).
//!
//! Builtins encode their `BuiltinId` as a fixed 7-bit field.
//!
//! ## Defensive properties
//!
//! 1. **Depth-limited.** Recursion is bounded by `FLAT_MAX_DEPTH` (set
//!    to mirror the Haskell `maxScriptSize` in bytes, since each level
//!    consumes at least 4 bits).
//! 2. **Allocation-clamped.** Every `Vec::with_capacity` call uses the
//!    same `safe_alloc_capacity` pattern as `dugite-serialization`.
//! 3. **No `unwrap`/`panic!`.** The decoder returns `UplcError::FlatDecode`
//!    for every truncated / malformed / out-of-range input.

#![allow(dead_code)] // pre-implementation scaffolding

pub mod bits;
pub mod decode;
pub mod encode;
pub mod term;

/// Maximum recursion depth for the flat decoder.
///
/// Haskell's flat decoder imposes NO depth cap — validity is bounded only by
/// the on-chain script-size limit (16 KiB). With at least 4 bits consumed per
/// tree-level the theoretical maximum depth for a 16 KiB script is
/// 16 × 1024 × 8 / 4 = 32 768, so a cap below that can reject a valid-but-deep
/// script that cardano-node accepts (a phase-2 conformance/liveness risk —
/// observed as the Alonzo "filler missing"/"term depth limit" divergence). We
/// therefore set the cap at the true 16 KiB ceiling. Stack exhaustion is no
/// longer the cap's concern: `stacker::maybe_grow` (see `decode_term_depth`)
/// transparently extends the OS stack at the recursion site, so heap/stack stay
/// bounded by the script size regardless of depth.
pub const FLAT_MAX_DEPTH: usize = 32768;

/// Result alias for flat decode/encode operations.
pub type FlatResult<T> = Result<T, crate::UplcError>;
