//! Flat-encoded UPLC decoder.
//!
//! Scaffolding only — the actual bit-stream decoder lands in the next
//! commit after the design synthesis. The entry point reserved here is:
//!
//! ```text
//! pub fn decode_term(bytes: &[u8]) -> FlatResult<Term>
//! pub fn decode_program(bytes: &[u8]) -> FlatResult<Program>
//! ```
//!
//! Defensive invariants the decoder must enforce (already captured as
//! tests in `tests/flat_decode_defense.rs` once written):
//!
//!  - Truncated input → `UplcError::FlatDecode("unexpected end of input")`.
//!  - Unknown term-tag → `UplcError::FlatDecode("unknown term tag {:#06b}")`.
//!  - Recursion depth past `FLAT_MAX_DEPTH` → `UplcError::FlatDecode("depth limit exceeded")`.
//!  - Builtin-id discriminant outside the known set → `UplcError::FlatDecode("unknown builtin id {n}")`.
//!  - `Vec::with_capacity(N)` calls are clamped to `min(N, remaining_bits/4)`.

#![allow(dead_code)]
