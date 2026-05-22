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

pub mod decode;
pub mod encode;

/// Maximum recursion depth for the flat decoder. The on-chain script
/// size limit is ~16 KiB; with 4-bit per constructor that's an upper
/// bound of ~32 K nodes, but real scripts are much shallower. We pick
/// a generous limit that protects against stack-exhaustion attacks.
pub const FLAT_MAX_DEPTH: usize = 4096;

/// Result alias for flat decode/encode operations.
pub type FlatResult<T> = Result<T, crate::UplcError>;
