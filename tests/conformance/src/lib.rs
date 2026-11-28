//! # Dugite Hand-written Ledger Conformance Vectors
//!
//! This crate runs a set of hand-written JSON test vectors against Dugite's
//! ledger implementation.
//!
//! ## Provenance
//!
//! The vectors in `vectors/` are NOT generated from the Agda formal ledger
//! specification: there is no Agda -> Haskell generator in this repository.
//! They were written by hand, and their expected outputs were edited to follow
//! code changes, so they are not an independent oracle.
//!
//! The mapping between the spec's abstract types and Dugite's concrete types is
//! handled by the [`adapters`] module.
//!
//! ## Supported Rules
//!
//! - **UTXO**: Transaction processing against UTxO state
//! - **CERT**: Certificate processing (delegation, registration)
//! - **GOV**: Governance actions (Conway era)
//! - **EPOCH**: Epoch boundary transitions

pub mod adapters;
pub mod node_diversity;
pub mod runner;
pub mod schema;

#[cfg(feature = "upstream-conformance")]
pub mod upstream;
