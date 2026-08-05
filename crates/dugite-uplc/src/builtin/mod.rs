//! Plutus Core builtin function dispatch.
//!
//! Every builtin has:
//!
//!  - A fixed [`crate::term::BuiltinId`] discriminant (matches the
//!    Haskell `DefaultFun` ordering verbatim — wire-compatible).
//!  - A *forces-and-args* signature: how many `Force`s must precede the
//!    first argument, and what type each argument must have.
//!  - A cost-model entry: a `CostingFun` per builtin keyed by the input
//!    sizes (constant / linear / quadratic depending on the builtin).
//!  - A *denotation*: the actual function, returning either a value or
//!    [`crate::UplcError::BuiltinFailure`].
//!
//! Bit-for-bit reproducibility against cardano-node requires:
//!
//!  - **Integer arithmetic** matches `Integer` (i.e. `BigInt`).
//!  - **ByteString** ops match Haskell `ByteString` (length-prefix
//!    is `Int64`; we treat it as `u64` with a hard cap).
//!  - **Sha2_256 / Sha3_256 / Blake2b_256 / Blake2b_224 / Keccak_256
//!    / Ripemd_160** use the exact reference algorithms.
//!  - **VerifyEd25519Signature** uses `ed25519-dalek`'s `verify_strict`.
//!    cardano-base implements Ed25519 DSIGN over libsodium's
//!    `crypto_sign_verify_detached`, which REJECTS small-order and
//!    non-canonical public keys and small-order `R`. The permissive
//!    `Verifier::verify` path accepts them: with `A` = identity,
//!    `R` = identity and `s` = 0 the cofactorless equation
//!    `[s]B = R + [k]A` degenerates to `identity = identity` and
//!    verification succeeds for ANY message. See #997 — do not relax
//!    this back to `verify`.
//!  - **VerifyEcdsaSecp256k1Signature / VerifySchnorrSecp256k1Signature**
//!    follow CIP-49.
//!  - **BLS12-381 ops** follow CIP-0381 (zkcrypto/IETF serialisation,
//!    BLS12381G[12]_XMD:SHA-256_SSWU_RO_ ciphersuites, strict subgroup
//!    checks). Implementation will use `blst` for performance.

#![allow(dead_code)]

pub mod arity;
pub mod bls;
pub mod cost;
pub mod denotations;
pub mod dispatch;
pub mod semantics;
