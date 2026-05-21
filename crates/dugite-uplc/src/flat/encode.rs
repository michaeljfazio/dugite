//! Flat-encoded UPLC encoder.
//!
//! Scaffolding only — see `decode.rs` for the symmetric inverse.
//!
//! Property test target: for every `Term` produced by the proptest
//! generator, `decode(encode(t))` round-trips and `encode(decode(b))`
//! is byte-identical on canonical inputs.

#![allow(dead_code)]
