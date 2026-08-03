//! Library surface of `dugite-cli`, exposed for fuzzing (issue #975).
//!
//! `dugite-cli` was a binary-only crate, so nothing outside it could reach the
//! key-material parsers — and those parse untrusted, user-supplied bytes with
//! a defect history. `envelope::unwrap_key_bytes` is the strict replacement
//! #935 introduced for four lenient CBOR unwrap heuristics, one of which (`&
//! 0xe0`) ate the first byte of any raw key starting `0x40..=0x5f` — a 1-in-8
//! silent corruption of key bytes. A strict replacement for a subtly-wrong
//! parser is precisely what should be pinned by a fuzz target, and it had none.
//!
//! Only the modules with untrusted-input parse surface are declared here. The
//! binary owns the full `mod commands;` tree and compiles it as a separate
//! unit, matching the pattern `dugite-node`'s lib target already uses.

/// Text-envelope and key-material parsing.
#[path = "commands/envelope.rs"]
pub mod envelope;
