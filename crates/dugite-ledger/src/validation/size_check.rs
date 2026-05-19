//! Uniform helper for fixed-size byte-field validation in ledger phase-1.
//!
//! # Rationale
//!
//! Pallas decodes wire types as `Vec<u8>`; dugite must enforce expected lengths
//! downstream before passing those bytes into cryptographic functions.  The
//! canonical pattern is:
//!
//! ```text
//! if X.len() == K { crypto_check } else { /* silent skip */ }
//! ```
//!
//! which silently accepts malformed inputs — the bug class first identified in
//! #537/#539.  This module provides a single, named helper so that every
//! crypto-input length check is visible, auditable, and returns an error
//! (never a silent pass) on size mismatch.
//!
//! # Usage by #546 (BootstrapWitness path)
//!
//! This module is intentionally shared. The `fix/audit-546-ledger-phase1`
//! branch uses `expect_size` for the BootstrapWitness XPub/XSignature fields.
//! Both branches add coverage via the `verify_single_witness` pre-flight gate.

use super::ValidationError;

/// Verify that a byte slice has exactly the expected length.
///
/// Returns `Ok(())` on success, or `Err(ValidationError::InvalidWitnessSignature)`
/// with a diagnostic message on failure.
///
/// The `field` parameter is the human-readable field name that appears in the
/// error message (e.g. `"vkey"`, `"signature"`, `"chain_code"`).  It is a
/// `&'static str` so callers cannot accidentally use a runtime string and
/// obscure the diagnostic.
#[inline]
pub(crate) fn expect_size(
    field: &'static str,
    actual: usize,
    expected: usize,
) -> Result<(), ValidationError> {
    if actual != expected {
        return Err(ValidationError::InvalidWitnessSignature(format!(
            "malformed {field}: expected {expected} bytes, got {actual}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expect_size_exact_match_ok() {
        assert!(expect_size("vkey", 32, 32).is_ok());
        assert!(expect_size("sig", 64, 64).is_ok());
        assert!(expect_size("chain_code", 32, 32).is_ok());
    }

    #[test]
    fn expect_size_too_short_err() {
        let e = expect_size("vkey", 1, 32).unwrap_err();
        let msg = format!("{e:?}");
        assert!(msg.contains("vkey"), "error must name the field");
        assert!(msg.contains("32"), "error must state expected size");
        assert!(msg.contains("1"), "error must state actual size");
    }

    #[test]
    fn expect_size_too_long_err() {
        let e = expect_size("signature", 65, 64).unwrap_err();
        let msg = format!("{e:?}");
        assert!(msg.contains("signature"));
        assert!(msg.contains("64"));
        assert!(msg.contains("65"));
    }

    #[test]
    fn expect_size_zero_err() {
        assert!(expect_size("vkey", 0, 32).is_err());
    }

    /// Length-lattice: for every boundary in {0, K-1, K, K+1, 100}
    /// only exactly K must return Ok.
    #[test]
    fn expect_size_lattice_vkey() {
        const K: usize = 32;
        for &sz in &[0_usize, K - 1, K, K + 1, 100] {
            let result = expect_size("vkey", sz, K);
            if sz == K {
                assert!(result.is_ok(), "size={sz} should be Ok");
            } else {
                assert!(result.is_err(), "size={sz} should be Err");
            }
        }
    }

    #[test]
    fn expect_size_lattice_sig() {
        const K: usize = 64;
        for &sz in &[0_usize, K - 1, K, K + 1, 200] {
            let result = expect_size("signature", sz, K);
            if sz == K {
                assert!(result.is_ok(), "size={sz} should be Ok");
            } else {
                assert!(result.is_err(), "size={sz} should be Err");
            }
        }
    }
}
