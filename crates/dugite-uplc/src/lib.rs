//! # dugite-uplc — Untyped Plutus Core for the dugite Cardano node
//!
//! First-party, in-house implementation of UPLC (Untyped Plutus Core), the
//! flat / CBOR wire format, the CEK machine, the Cardano-specific builtin
//! suite, the V1/V2/V3 script-context construction, and the phase-2
//! transaction evaluator.
//!
//! This crate replaces aiken-lang/uplc (and removes the transitive
//! `pallas-*` dependency chain that comes with it).
//!
//! ## Status: scaffolding
//!
//! This crate currently exposes only module skeletons. See `DESIGN.md` for
//! the implementation plan distilled from a cross-implementation study of:
//!
//!  - IntersectMBO/plutus (Haskell — the reference)
//!  - pragma-org/uplc (`amaru-uplc`, arena-allocated CEK)
//!  - aiken-lang/uplc (current Rust implementation, slated for removal)
//!  - The official Plutus Core formal specification + relevant CIPs
//!
//! ## Hard requirements
//!
//! 1. **Panic-free on adversarial input.** Every public API must return
//!    `Result`. No `unwrap`, no `expect`, no `panic!`, no `todo!`, no
//!    `unimplemented!`. Adversaries control the bytes via gossiped tx
//!    witness sets — a panic is a DoS bug.
//! 2. **Bounded allocation.** Every peer-supplied length header must be
//!    sanity-clamped before any `Vec::with_capacity` / `Vec::reserve`.
//! 3. **Bounded recursion.** CEK machine state must be heap-resident with
//!    explicit depth limits; the flat decoder and `Data` decoder must
//!    have explicit depth caps.
//! 4. **Bit-for-bit byte-exact compatibility** with cardano-node Haskell:
//!    flat-encoded scripts and CBOR-encoded `Data` round-trip identically;
//!    CEK evaluation produces identical `EvalResult` for identical inputs;
//!    BLS12-381, sha2_256, sha3_256, blake2b_224, blake2b_256, keccak_256,
//!    ripemd_160 builtins are bit-exact against the reference.
//! 5. **No third-party UPLC dependencies.** No aiken-uplc, no pallas-*,
//!    no amaru-uplc. Only standard Rust crypto crates (`blake2`, `sha2`,
//!    `sha3`, `secp256k1`, `blst`) and `minicbor` are linked.

#![deny(missing_debug_implementations)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
#![cfg_attr(not(test), deny(clippy::panic, clippy::todo, clippy::unimplemented))]

pub mod builtin;
pub mod cost_models;
pub mod data;
pub mod eval_redeemer;
pub mod flat;
pub mod machine;
pub mod phase_two;
pub mod populate_gov;
pub mod populate_v1_v2;
pub mod populate_v3;
pub mod program;
pub mod redeemer_resolve;
pub mod script_context;
pub mod term;
pub mod tx_info;
pub mod tx_info_populate;

mod error;

pub use crate::data::Data;
pub use crate::error::UplcError;
pub use crate::program::Program;
pub use crate::term::{Constant, Term};
