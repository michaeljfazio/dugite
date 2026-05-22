//! UPLC `Program` — the outer wrapper around a `Term`.
//!
//! A program is `(program major.minor.patch term)`. The on-chain wire
//! shape is:
//!
//!  1. A CBOR byte-string (major type 2) wrapping the flat-encoded
//!     program bytes.
//!  2. Inside the flat layer: the version triple as three `Natural`s,
//!     followed by the term.
//!
//! This module handles the CBOR ↔ flat-bytes boundary; the flat ↔ AST
//! boundary lives in `crate::flat`.

use crate::term::Term;

/// A complete UPLC program: a language version triple plus a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub version: (u64, u64, u64),
    pub term: Term,
}

impl Program {
    /// Decode a CBOR-wrapped flat-encoded UPLC program.
    pub fn from_cbor(_bytes: &[u8]) -> Result<Self, crate::UplcError> {
        Err(crate::UplcError::Internal(
            "Program::from_cbor not yet implemented".to_string(),
        ))
    }

    /// Decode a raw flat-encoded UPLC program (no CBOR wrapper).
    pub fn from_flat(_bytes: &[u8]) -> Result<Self, crate::UplcError> {
        Err(crate::UplcError::Internal(
            "Program::from_flat not yet implemented".to_string(),
        ))
    }

    /// Encode the program as `(major, minor, patch, flat-encoded term)`
    /// then wrap in a CBOR byte-string.
    pub fn to_cbor(&self) -> Result<Vec<u8>, crate::UplcError> {
        Err(crate::UplcError::Internal(
            "Program::to_cbor not yet implemented".to_string(),
        ))
    }

    /// Encode the program as raw flat bytes (no CBOR wrapper).
    pub fn to_flat(&self) -> Result<Vec<u8>, crate::UplcError> {
        Err(crate::UplcError::Internal(
            "Program::to_flat not yet implemented".to_string(),
        ))
    }
}
