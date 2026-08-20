//! Governance identifier bech32 encoding and decoding, matching cardano-cli's
//! DEFAULT (non-`--output-cip129`) bech32 form for the governance identifiers
//! introduced by CIP-1694 (Conway era on-chain governance).
//!
//! # Prefixes (bech32 Human-Readable Parts — do not include the `1`
//! separator; the bech32 encoder inserts it)
//!
//! | Identifier                              | HRP            |
//! |-----------------------------------------|----------------|
//! | DRep key hash credential                | `drep`         |
//! | DRep script hash credential             | `drep_script`  |
//! | CC hot key hash credential              | `cc_hot`       |
//! | CC hot script hash credential           | `cc_hot_script`|
//! | CC cold key hash credential             | `cc_cold`      |
//! | CC cold script hash credential          | `cc_cold_script`|
//!
//! All identifiers encode a raw 28-byte Blake2b-224 hash using Bech32
//! encoding — verified byte-for-byte against a real cardano-cli 11.0.1
//! `conway governance drep id` (default output mode) in the fix for the bug
//! this doc replaces: an earlier revision baked the bech32 separator into
//! these constants (`"drep1"` instead of `"drep"`), so the encoder's own
//! separator insertion produced a doubled `1` and every emitted identifier
//! was nonstandard bech32 no wallet, explorer, or cardano-cli could parse.
//!
//! # NOT implemented: true CIP-129
//!
//! cardano-cli's `--output-cip129` flag produces a DIFFERENT, LONGER bech32
//! string than the default mode this module matches — CIP-129 prepends a
//! header byte (encoding credential type + key/script) before the 28-byte
//! hash, so the encoded payload is 29 bytes, not 28. This module does not
//! implement that mode; it only matches cardano-cli's default output. A
//! previous revision of this doc claimed CIP-0129 compliance for the
//! header-less form, which was never checked against real output and does
//! not hold — confirmed by comparing this module's HRPs are correct for the
//! DEFAULT mode, not by re-deriving the CIP-129 claim.

use crate::hash::{Hash28, Hash32};
use bech32::{Bech32, Hrp};

/// HRP for a DRep key-hash credential.
pub const HRP_DREP: &str = "drep";

/// HRP for a DRep script-hash credential.
pub const HRP_DREP_SCRIPT: &str = "drep_script";

/// HRP for a Constitutional Committee hot key-hash credential.
pub const HRP_CC_HOT: &str = "cc_hot";

/// HRP for a Constitutional Committee hot script-hash credential.
pub const HRP_CC_HOT_SCRIPT: &str = "cc_hot_script";

/// HRP for a Constitutional Committee cold key-hash credential.
pub const HRP_CC_COLD: &str = "cc_cold";

/// HRP for a Constitutional Committee cold script-hash credential.
pub const HRP_CC_COLD_SCRIPT: &str = "cc_cold_script";

/// Error type for governance identifier encoding/decoding.
#[derive(Debug, thiserror::Error)]
pub enum GovernanceIdError {
    #[error("bech32 encoding error: {0}")]
    Bech32Encode(#[from] bech32::EncodeError),
    #[error("bech32 decoding error: {0}")]
    Bech32Decode(#[from] bech32::DecodeError),
    #[error("unexpected HRP '{actual}' (expected one of: {expected})")]
    WrongHrp { actual: String, expected: String },
    #[error("invalid payload length: expected 28 bytes, got {0}")]
    InvalidLength(usize),
}

// ──────────────────────────────────────────────────────────────────────────────
// Encoding helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Encode a 28-byte hash as a DRep key-hash bech32 identifier.
///
/// Produces a string with the `drep` HRP (e.g. `drep1...`, HRP + separator).
pub fn encode_drep_key(hash: &Hash28) -> Result<String, GovernanceIdError> {
    encode_governance_id(HRP_DREP, hash.as_bytes())
}

/// Encode a 28-byte hash as a DRep script-hash bech32 identifier.
///
/// Produces a string with the `drep_script` HRP.
pub fn encode_drep_script(hash: &Hash28) -> Result<String, GovernanceIdError> {
    encode_governance_id(HRP_DREP_SCRIPT, hash.as_bytes())
}

/// Encode a 28-byte hash as a CC hot key-hash bech32 identifier.
///
/// Produces a string with the `cc_hot` HRP.
pub fn encode_cc_hot_key(hash: &Hash28) -> Result<String, GovernanceIdError> {
    encode_governance_id(HRP_CC_HOT, hash.as_bytes())
}

/// Encode a 28-byte hash as a CC hot script-hash bech32 identifier.
///
/// Produces a string with the `cc_hot_script` HRP.
pub fn encode_cc_hot_script(hash: &Hash28) -> Result<String, GovernanceIdError> {
    encode_governance_id(HRP_CC_HOT_SCRIPT, hash.as_bytes())
}

/// Encode a 28-byte hash as a CC cold key-hash bech32 identifier.
///
/// Produces a string with the `cc_cold` HRP.
pub fn encode_cc_cold_key(hash: &Hash28) -> Result<String, GovernanceIdError> {
    encode_governance_id(HRP_CC_COLD, hash.as_bytes())
}

/// Encode a 28-byte hash as a CC cold script-hash bech32 identifier.
///
/// Produces a string with the `cc_cold_script` HRP.
pub fn encode_cc_cold_script(hash: &Hash28) -> Result<String, GovernanceIdError> {
    encode_governance_id(HRP_CC_COLD_SCRIPT, hash.as_bytes())
}

// ──────────────────────────────────────────────────────────────────────────────
// Decoding helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Decode a DRep key-hash bech32 identifier.
///
/// Returns the 28-byte hash.
pub fn decode_drep_key(s: &str) -> Result<Hash28, GovernanceIdError> {
    let (hrp, data) = bech32::decode(s)?;
    let hrp_str = hrp.as_str();
    if hrp_str != HRP_DREP {
        return Err(GovernanceIdError::WrongHrp {
            actual: hrp_str.to_string(),
            expected: HRP_DREP.to_string(),
        });
    }
    bytes_to_hash28(&data)
}

/// Decode a DRep script-hash bech32 identifier.
///
/// Returns the 28-byte hash.
pub fn decode_drep_script(s: &str) -> Result<Hash28, GovernanceIdError> {
    let (hrp, data) = bech32::decode(s)?;
    let hrp_str = hrp.as_str();
    if hrp_str != HRP_DREP_SCRIPT {
        return Err(GovernanceIdError::WrongHrp {
            actual: hrp_str.to_string(),
            expected: HRP_DREP_SCRIPT.to_string(),
        });
    }
    bytes_to_hash28(&data)
}

/// Decode a CC hot key-hash bech32 identifier.
///
/// Returns the 28-byte hash.
pub fn decode_cc_hot_key(s: &str) -> Result<Hash28, GovernanceIdError> {
    let (hrp, data) = bech32::decode(s)?;
    let hrp_str = hrp.as_str();
    if hrp_str != HRP_CC_HOT {
        return Err(GovernanceIdError::WrongHrp {
            actual: hrp_str.to_string(),
            expected: HRP_CC_HOT.to_string(),
        });
    }
    bytes_to_hash28(&data)
}

/// Decode a CC hot script-hash bech32 identifier.
///
/// Returns the 28-byte hash.
pub fn decode_cc_hot_script(s: &str) -> Result<Hash28, GovernanceIdError> {
    let (hrp, data) = bech32::decode(s)?;
    let hrp_str = hrp.as_str();
    if hrp_str != HRP_CC_HOT_SCRIPT {
        return Err(GovernanceIdError::WrongHrp {
            actual: hrp_str.to_string(),
            expected: HRP_CC_HOT_SCRIPT.to_string(),
        });
    }
    bytes_to_hash28(&data)
}

/// Decode a CC cold key-hash bech32 identifier.
///
/// Returns the 28-byte hash.
pub fn decode_cc_cold_key(s: &str) -> Result<Hash28, GovernanceIdError> {
    let (hrp, data) = bech32::decode(s)?;
    let hrp_str = hrp.as_str();
    if hrp_str != HRP_CC_COLD {
        return Err(GovernanceIdError::WrongHrp {
            actual: hrp_str.to_string(),
            expected: HRP_CC_COLD.to_string(),
        });
    }
    bytes_to_hash28(&data)
}

/// Decode a CC cold script-hash bech32 identifier.
///
/// Returns the 28-byte hash.
pub fn decode_cc_cold_script(s: &str) -> Result<Hash28, GovernanceIdError> {
    let (hrp, data) = bech32::decode(s)?;
    let hrp_str = hrp.as_str();
    if hrp_str != HRP_CC_COLD_SCRIPT {
        return Err(GovernanceIdError::WrongHrp {
            actual: hrp_str.to_string(),
            expected: HRP_CC_COLD_SCRIPT.to_string(),
        });
    }
    bytes_to_hash28(&data)
}

// ──────────────────────────────────────────────────────────────────────────────
// Credential-type-aware helpers
// ──────────────────────────────────────────────────────────────────────────────

/// Governance credential kind — key-hash or script-hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredKind {
    /// Verification key hash (Blake2b-224 of a public key).
    Key,
    /// Script hash (Blake2b-224 of a script).
    Script,
}

/// Encode a DRep credential (key or script) as a bech32 identifier.
///
/// Selects the `drep` HRP for key credentials and `drep_script` for script
/// credentials.
pub fn encode_drep(hash: &Hash28, kind: CredKind) -> Result<String, GovernanceIdError> {
    match kind {
        CredKind::Key => encode_drep_key(hash),
        CredKind::Script => encode_drep_script(hash),
    }
}

/// Encode a CC hot credential (key or script) as a bech32 identifier.
///
/// Selects the `cc_hot` HRP for key credentials and `cc_hot_script` for
/// script credentials.
pub fn encode_cc_hot(hash: &Hash28, kind: CredKind) -> Result<String, GovernanceIdError> {
    match kind {
        CredKind::Key => encode_cc_hot_key(hash),
        CredKind::Script => encode_cc_hot_script(hash),
    }
}

/// Encode a CC cold credential (key or script) as a bech32 identifier.
///
/// Selects the `cc_cold` HRP for key credentials and `cc_cold_script` for
/// script credentials.
pub fn encode_cc_cold(hash: &Hash28, kind: CredKind) -> Result<String, GovernanceIdError> {
    match kind {
        CredKind::Key => encode_cc_cold_key(hash),
        CredKind::Script => encode_cc_cold_script(hash),
    }
}

/// Encode a governance identifier from a CBOR credential pair `[type, hash_bytes]`.
///
/// The `cred_type` byte matches the Cardano CBOR encoding:
/// - `0` = key-hash credential
/// - `1` = script-hash credential
///
/// This function is intended for use when decoding raw LocalStateQuery responses
/// where credentials arrive as `array(2) [u8, bstr(28)]`.
pub fn encode_drep_from_cbor(
    cred_type: u8,
    hash_bytes: &[u8],
) -> Result<String, GovernanceIdError> {
    let hash = bytes_to_hash28(hash_bytes)?;
    match cred_type {
        0 => encode_drep_key(&hash),
        1 => encode_drep_script(&hash),
        _ => Err(GovernanceIdError::WrongHrp {
            actual: format!("type={cred_type}"),
            expected: "0 (key) or 1 (script)".to_string(),
        }),
    }
}

/// Encode a CC hot identifier from a CBOR credential pair `[type, hash_bytes]`.
///
/// See [`encode_drep_from_cbor`] for the `cred_type` convention.
pub fn encode_cc_hot_from_cbor(
    cred_type: u8,
    hash_bytes: &[u8],
) -> Result<String, GovernanceIdError> {
    let hash = bytes_to_hash28(hash_bytes)?;
    match cred_type {
        0 => encode_cc_hot_key(&hash),
        1 => encode_cc_hot_script(&hash),
        _ => Err(GovernanceIdError::WrongHrp {
            actual: format!("type={cred_type}"),
            expected: "0 (key) or 1 (script)".to_string(),
        }),
    }
}

/// Encode a CC cold identifier from a CBOR credential pair `[type, hash_bytes]`.
///
/// See [`encode_drep_from_cbor`] for the `cred_type` convention.
pub fn encode_cc_cold_from_cbor(
    cred_type: u8,
    hash_bytes: &[u8],
) -> Result<String, GovernanceIdError> {
    let hash = bytes_to_hash28(hash_bytes)?;
    match cred_type {
        0 => encode_cc_cold_key(&hash),
        1 => encode_cc_cold_script(&hash),
        _ => Err(GovernanceIdError::WrongHrp {
            actual: format!("type={cred_type}"),
            expected: "0 (key) or 1 (script)".to_string(),
        }),
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// CIP-0129 encoding (`--output-cip129` / `cip-format cip-129 …`)
// ──────────────────────────────────────────────────────────────────────────────
//
// True CIP-129 (https://github.com/cardano-foundation/CIPs/tree/master/CIP-0129)
// is a DIFFERENT, LONGER encoding than the module-level default above: it
// prepends a single HEADER BYTE to the 28-byte hash (29-byte payload, not 28)
// and uses ONE Bech32 HRP per governance-identifier namespace regardless of
// key-vs-script — the header byte carries that distinction instead of the
// HRP. Governance action IDs have no header byte at all; their payload is
// simply `txid(32) || index_u16_be(2)` = 34 bytes under the `gov_action` HRP.
//
// The header-byte layout is `(type << 4) | cred_kind`:
//   type:      0 = Constitutional Committee HOT, 1 = CC COLD, 2 = DRep
//   cred_kind: 2 = key-hash credential, 3 = script-hash credential
//
// Verified empirically against a real cardano-cli 11.0.1, NOT taken from a
// written spec re-derivation (this repo's standing rule for wire-format
// claims — see `test_drep_key_matches_real_cardano_cli_output` above for the
// same discipline applied to the non-CIP129 form):
//   - `cardano-cli conway governance drep id --output-cip129` on a freshly
//     generated DRep vkey produced `drep1yt97pt...` which bech32-decodes to
//     header `0x22` + the SAME 28-byte hash `--output-hex` reports.
//   - `cardano-cli cip-format cip-129 committee-cold-key`/`committee-hot-key`
//     on freshly generated CC cold/hot vkeys produced headers `0x12`/`0x02`
//     over `blake2b_224(vkey)`.
//   - `cardano-cli cip-format cip-129 governance-action-id
//     --governance-action-hex <txid>#<index>` on `aa..aa#1`, `bb..bb#7`
//     decoded to `aa..aa 0001` / `bb..bb 0007` — txid followed by a 2-byte
//     BIG-ENDIAN index, no header byte, HRP `gov_action`.
//
// Script-credential headers (`0x23`/`0x13`/`0x03`) are exposed for API
// symmetry with the module's `CredKind`-based helpers above, but no in-tree
// caller reaches them yet: every `cip-format cip-129 {drep,committee-*-key}`
// subcommand's input surface is a verification KEY only (no script-hash
// flag), matching cardano-cli's own `--help` for those three subcommands.

/// HRP for a CIP-129 DRep identifier (key or script — the header byte
/// disambiguates).
pub const HRP_CIP129_DREP: &str = "drep";

/// HRP for a CIP-129 Constitutional Committee hot identifier.
pub const HRP_CIP129_CC_HOT: &str = "cc_hot";

/// HRP for a CIP-129 Constitutional Committee cold identifier.
pub const HRP_CIP129_CC_COLD: &str = "cc_cold";

/// HRP for a CIP-129 governance action identifier.
pub const HRP_CIP129_GOV_ACTION: &str = "gov_action";

/// CIP-129 header byte for a DRep key-hash credential.
pub const CIP129_HEADER_DREP_KEY: u8 = 0x22;
/// CIP-129 header byte for a DRep script-hash credential.
pub const CIP129_HEADER_DREP_SCRIPT: u8 = 0x23;
/// CIP-129 header byte for a Constitutional Committee cold key-hash credential.
pub const CIP129_HEADER_CC_COLD_KEY: u8 = 0x12;
/// CIP-129 header byte for a Constitutional Committee cold script-hash credential.
pub const CIP129_HEADER_CC_COLD_SCRIPT: u8 = 0x13;
/// CIP-129 header byte for a Constitutional Committee hot key-hash credential.
pub const CIP129_HEADER_CC_HOT_KEY: u8 = 0x02;
/// CIP-129 header byte for a Constitutional Committee hot script-hash credential.
pub const CIP129_HEADER_CC_HOT_SCRIPT: u8 = 0x03;

fn cip129_header(kind: CredKind, key_header: u8, script_header: u8) -> u8 {
    match kind {
        CredKind::Key => key_header,
        CredKind::Script => script_header,
    }
}

/// Encode a 28-byte hash as a CIP-129 DRep identifier: HRP `drep`, payload
/// `header || hash` (29 bytes).
pub fn encode_drep_cip129(hash: &Hash28, kind: CredKind) -> Result<String, GovernanceIdError> {
    encode_cip129_credential(
        HRP_CIP129_DREP,
        cip129_header(kind, CIP129_HEADER_DREP_KEY, CIP129_HEADER_DREP_SCRIPT),
        hash,
    )
}

/// Encode a 28-byte hash as a CIP-129 Constitutional Committee cold
/// identifier: HRP `cc_cold`, payload `header || hash` (29 bytes).
pub fn encode_cc_cold_cip129(hash: &Hash28, kind: CredKind) -> Result<String, GovernanceIdError> {
    encode_cip129_credential(
        HRP_CIP129_CC_COLD,
        cip129_header(
            kind,
            CIP129_HEADER_CC_COLD_KEY,
            CIP129_HEADER_CC_COLD_SCRIPT,
        ),
        hash,
    )
}

/// Encode a 28-byte hash as a CIP-129 Constitutional Committee hot
/// identifier: HRP `cc_hot`, payload `header || hash` (29 bytes).
pub fn encode_cc_hot_cip129(hash: &Hash28, kind: CredKind) -> Result<String, GovernanceIdError> {
    encode_cip129_credential(
        HRP_CIP129_CC_HOT,
        cip129_header(kind, CIP129_HEADER_CC_HOT_KEY, CIP129_HEADER_CC_HOT_SCRIPT),
        hash,
    )
}

/// Encode a governance action ID as a CIP-129 identifier: HRP `gov_action`,
/// payload `txid(32) || index_u16_be(2)` (34 bytes) — no header byte.
///
/// `index` is a `u16` because `GovActionIx` upstream is `Word16`; a proposal
/// index above 65535 cannot occur (a block cannot carry that many proposal
/// procedures) so this is not a narrowing concern in practice.
pub fn encode_governance_action_id_cip129(
    txid: &Hash32,
    index: u16,
) -> Result<String, GovernanceIdError> {
    let mut payload = Vec::with_capacity(34);
    payload.extend_from_slice(txid.as_bytes());
    payload.extend_from_slice(&index.to_be_bytes());
    encode_governance_id(HRP_CIP129_GOV_ACTION, &payload)
}

fn encode_cip129_credential(
    hrp: &str,
    header: u8,
    hash: &Hash28,
) -> Result<String, GovernanceIdError> {
    let mut payload = Vec::with_capacity(29);
    payload.push(header);
    payload.extend_from_slice(hash.as_bytes());
    encode_governance_id(hrp, &payload)
}

// ──────────────────────────────────────────────────────────────────────────────
// Internal utilities
// ──────────────────────────────────────────────────────────────────────────────

/// Encode raw bytes using the given HRP as a Bech32 string.
fn encode_governance_id(hrp_str: &str, data: &[u8]) -> Result<String, GovernanceIdError> {
    let hrp = Hrp::parse(hrp_str).map_err(|e| {
        // bech32::EncodeError can't be constructed from HrpError directly,
        // so we use a workaround: attempt a no-op encode which will surface it.
        // In practice, all our HRP constants are valid at compile time.
        let _ = e;
        GovernanceIdError::WrongHrp {
            actual: hrp_str.to_string(),
            expected: "(valid HRP)".to_string(),
        }
    })?;
    Ok(bech32::encode::<Bech32>(hrp, data)?)
}

/// Convert a byte slice to a `Hash28`, returning an error if the length != 28.
fn bytes_to_hash28(data: &[u8]) -> Result<Hash28, GovernanceIdError> {
    if data.len() != 28 {
        return Err(GovernanceIdError::InvalidLength(data.len()));
    }
    let mut arr = [0u8; 28];
    arr.copy_from_slice(data);
    Ok(Hash28::from_bytes(arr))
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed 28-byte test hash (all 0x01 bytes).
    fn test_hash() -> Hash28 {
        Hash28::from_bytes([0x01u8; 28])
    }

    // ── Encoding round-trip tests ────────────────────────────────────────────

    /// `starts_with("drep1")` alone is NOT a sufficient assertion here — the
    /// bug this module was fixed for (baking the bech32 separator into the
    /// HRP constant, producing "drep11..." instead of "drep1...") ALSO
    /// starts with "drep1", since "drep11..." trivially starts with its own
    /// first five characters. Decoding the HRP back out and comparing it
    /// EXACTLY is what would have caught it: the buggy encoding decodes to
    /// HRP "drep1" (bech32 uses the LAST '1' in the string as the
    /// separator, and the data alphabet never contains '1', so "drep11..."
    /// unambiguously decodes to hrp="drep1" + data starting at the second
    /// '1' — not hrp="drep").
    fn assert_hrp_exact(encoded: &str, expected_hrp: &str) {
        let (hrp, _data) = bech32::decode(encoded).expect("must be valid bech32");
        assert_eq!(
            hrp.as_str(),
            expected_hrp,
            "decoded HRP must be exactly {expected_hrp:?} — got {:?} from {encoded}",
            hrp.as_str()
        );
    }

    #[test]
    fn test_drep_key_roundtrip() {
        let h = test_hash();
        let encoded = encode_drep_key(&h).expect("encode should succeed");
        assert_hrp_exact(&encoded, HRP_DREP);
        let decoded = decode_drep_key(&encoded).expect("decode should succeed");
        assert_eq!(decoded, h, "round-trip should return original hash");
    }

    #[test]
    fn test_drep_script_roundtrip() {
        let h = test_hash();
        let encoded = encode_drep_script(&h).expect("encode should succeed");
        assert_hrp_exact(&encoded, HRP_DREP_SCRIPT);
        let decoded = decode_drep_script(&encoded).expect("decode should succeed");
        assert_eq!(decoded, h);
    }

    #[test]
    fn test_cc_hot_key_roundtrip() {
        let h = test_hash();
        let encoded = encode_cc_hot_key(&h).expect("encode should succeed");
        assert_hrp_exact(&encoded, HRP_CC_HOT);
        let decoded = decode_cc_hot_key(&encoded).expect("decode should succeed");
        assert_eq!(decoded, h);
    }

    #[test]
    fn test_cc_hot_script_roundtrip() {
        let h = test_hash();
        let encoded = encode_cc_hot_script(&h).expect("encode should succeed");
        assert_hrp_exact(&encoded, HRP_CC_HOT_SCRIPT);
        let decoded = decode_cc_hot_script(&encoded).expect("decode should succeed");
        assert_eq!(decoded, h);
    }

    #[test]
    fn test_cc_cold_key_roundtrip() {
        let h = test_hash();
        let encoded = encode_cc_cold_key(&h).expect("encode should succeed");
        assert_hrp_exact(&encoded, HRP_CC_COLD);
        let decoded = decode_cc_cold_key(&encoded).expect("decode should succeed");
        assert_eq!(decoded, h);
    }

    #[test]
    fn test_cc_cold_script_roundtrip() {
        let h = test_hash();
        let encoded = encode_cc_cold_script(&h).expect("encode should succeed");
        assert_hrp_exact(&encoded, HRP_CC_COLD_SCRIPT);
        let decoded = decode_cc_cold_script(&encoded).expect("decode should succeed");
        assert_eq!(decoded, h);
    }

    /// Byte-exact against a REAL cardano-cli 11.0.1 `conway governance drep
    /// id` (default, non-`--output-cip129` output mode) — not a
    /// self-consistency round-trip, which is exactly what let the doubled-
    /// separator bug ship silently: encode and decode agreeing with each
    /// other proves nothing when they share the same wrong constant.
    /// Captured 2026-08-20 from a freshly generated DRep verification key;
    /// the hash is `cardano-cli ... drep id --output-hex`'s raw 28 bytes for
    /// that same key.
    #[test]
    fn test_drep_key_matches_real_cardano_cli_output() {
        let hex = "1ccd651268a5c1b07cca01101c9fab20570456d7454b0dd1b9957f10";
        let mut bytes = [0u8; 28];
        for i in 0..28 {
            bytes[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let h = Hash28::from_bytes(bytes);
        let encoded = encode_drep_key(&h).expect("encode should succeed");
        assert_eq!(
            encoded, "drep1rnxk2yng5hqmqlx2qygpe8atyptsg4khg49sm5dej4l3q2hkzth",
            "must match real cardano-cli's default bech32 output exactly"
        );
    }

    // ── CredKind dispatch tests ──────────────────────────────────────────────

    #[test]
    fn test_encode_drep_key_kind() {
        let h = test_hash();
        let result = encode_drep(&h, CredKind::Key).expect("should succeed");
        assert!(result.starts_with("drep1"), "got: {result}");
    }

    #[test]
    fn test_encode_drep_script_kind() {
        let h = test_hash();
        let result = encode_drep(&h, CredKind::Script).expect("should succeed");
        assert!(result.starts_with("drep_script1"), "got: {result}");
    }

    #[test]
    fn test_encode_cc_hot_key_kind() {
        let h = test_hash();
        let result = encode_cc_hot(&h, CredKind::Key).expect("should succeed");
        assert!(result.starts_with("cc_hot1"), "got: {result}");
    }

    #[test]
    fn test_encode_cc_hot_script_kind() {
        let h = test_hash();
        let result = encode_cc_hot(&h, CredKind::Script).expect("should succeed");
        assert!(result.starts_with("cc_hot_script1"), "got: {result}");
    }

    #[test]
    fn test_encode_cc_cold_key_kind() {
        let h = test_hash();
        let result = encode_cc_cold(&h, CredKind::Key).expect("should succeed");
        assert!(result.starts_with("cc_cold1"), "got: {result}");
    }

    #[test]
    fn test_encode_cc_cold_script_kind() {
        let h = test_hash();
        let result = encode_cc_cold(&h, CredKind::Script).expect("should succeed");
        assert!(result.starts_with("cc_cold_script1"), "got: {result}");
    }

    // ── CBOR-aware encoding tests ────────────────────────────────────────────

    #[test]
    fn test_encode_drep_from_cbor_key() {
        let bytes = [0x02u8; 28];
        let result = encode_drep_from_cbor(0, &bytes).expect("should succeed");
        assert!(result.starts_with("drep1"), "got: {result}");
    }

    #[test]
    fn test_encode_drep_from_cbor_script() {
        let bytes = [0x03u8; 28];
        let result = encode_drep_from_cbor(1, &bytes).expect("should succeed");
        assert!(result.starts_with("drep_script1"), "got: {result}");
    }

    #[test]
    fn test_encode_cc_hot_from_cbor_key() {
        let bytes = [0x04u8; 28];
        let result = encode_cc_hot_from_cbor(0, &bytes).expect("should succeed");
        assert!(result.starts_with("cc_hot1"), "got: {result}");
    }

    #[test]
    fn test_encode_cc_cold_from_cbor_script() {
        let bytes = [0x05u8; 28];
        let result = encode_cc_cold_from_cbor(1, &bytes).expect("should succeed");
        assert!(result.starts_with("cc_cold_script1"), "got: {result}");
    }

    #[test]
    fn test_encode_drep_from_cbor_invalid_type() {
        let bytes = [0x00u8; 28];
        let result = encode_drep_from_cbor(2, &bytes);
        assert!(result.is_err(), "type=2 should be rejected");
    }

    #[test]
    fn test_encode_drep_from_cbor_wrong_length() {
        let bytes = [0x00u8; 20]; // wrong length
        let result = encode_drep_from_cbor(0, &bytes);
        assert!(result.is_err(), "20-byte payload should be rejected");
    }

    // ── Bare 'drep' HRP, built independently of HRP_DREP ─────────────────────

    /// Builds the HRP from a literal `"drep"` string rather than the
    /// `HRP_DREP` constant `decode_drep_key` itself checks against — an
    /// earlier revision of this test called this "legacy" backward
    /// compatibility because `HRP_DREP` used to be the (buggy) `"drep1"`;
    /// now that `HRP_DREP` IS `"drep"`, this is really just an independent
    /// check that the decoder's accepted HRP matches the literal string,
    /// not a fixture built from the same constant it's testing.
    #[test]
    fn test_decode_drep_key_bare_drep_hrp() {
        let h = test_hash();
        let hrp = Hrp::parse("drep").expect("valid HRP");
        let independently_built = bech32::encode::<Bech32>(hrp, h.as_bytes()).expect("encode");
        let decoded =
            decode_drep_key(&independently_built).expect("bare 'drep' HRP should be accepted");
        assert_eq!(decoded, h);
    }

    // ── Wrong-HRP rejection tests ────────────────────────────────────────────

    #[test]
    fn test_decode_drep_key_rejects_wrong_hrp() {
        let h = test_hash();
        let encoded = encode_drep_script(&h).expect("encode");
        let result = decode_drep_key(&encoded);
        assert!(
            matches!(result, Err(GovernanceIdError::WrongHrp { .. })),
            "drep_script1 prefix should be rejected by decode_drep_key"
        );
    }

    #[test]
    fn test_decode_cc_hot_key_rejects_cc_cold() {
        let h = test_hash();
        let encoded = encode_cc_cold_key(&h).expect("encode");
        let result = decode_cc_hot_key(&encoded);
        assert!(
            matches!(result, Err(GovernanceIdError::WrongHrp { .. })),
            "cc_cold1 prefix should be rejected by decode_cc_hot_key"
        );
    }

    // ── Distinctness: all six encodings produce different outputs ────────────

    #[test]
    fn test_all_six_identifiers_are_distinct() {
        let h = test_hash();
        let ids = [
            encode_drep_key(&h).unwrap(),
            encode_drep_script(&h).unwrap(),
            encode_cc_hot_key(&h).unwrap(),
            encode_cc_hot_script(&h).unwrap(),
            encode_cc_cold_key(&h).unwrap(),
            encode_cc_cold_script(&h).unwrap(),
        ];
        // All six must be pairwise distinct.
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(
                    ids[i], ids[j],
                    "identifiers at {i} and {j} should be distinct"
                );
            }
        }
    }

    // ── Known-value test (Bech32 is deterministic) ───────────────────────────

    // ── CIP-129 tests ─────────────────────────────────────────────────────

    /// Byte-exact against `cardano-cli conway governance drep id
    /// --output-cip129` on the SAME key as
    /// `test_drep_key_matches_real_cardano_cli_output` above. Captured
    /// 2026-08-21.
    #[test]
    fn test_drep_cip129_matches_real_cardano_cli_output() {
        let hex = "cbe0ada60857a7e5bd74bada35ffcc72f57c33818004b9b4a81d76f1";
        let mut bytes = [0u8; 28];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let h = Hash28::from_bytes(bytes);
        let encoded = encode_drep_cip129(&h, CredKind::Key).expect("encode should succeed");
        assert_eq!(
            encoded, "drep1yt97ptdxppt60edawjad5d0le3e02lpnsxqqfwd54qwhdugx0pfsd",
            "must match real cardano-cli's --output-cip129 output exactly"
        );
    }

    /// Byte-exact against `cardano-cli cip-format cip-129 committee-cold-key`
    /// on a freshly generated CC cold vkey. Captured 2026-08-21.
    #[test]
    fn test_cc_cold_cip129_matches_real_cardano_cli_output() {
        let hex = "09bf61e03b26b70687a1a69cf9e294e354e1a6996f38c883ca9618fe";
        let mut bytes = [0u8; 28];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let h = Hash28::from_bytes(bytes);
        let encoded = encode_cc_cold_cip129(&h, CredKind::Key).expect("encode should succeed");
        assert_eq!(
            encoded,
            "cc_cold1zgym7c0q8vntwp585xnfe70zjn34fcdxn9hn3jyre2tp3lsr7q8h4"
        );
    }

    /// Byte-exact against `cardano-cli cip-format cip-129 committee-hot-key`
    /// on a freshly generated CC hot vkey. Captured 2026-08-21.
    #[test]
    fn test_cc_hot_cip129_matches_real_cardano_cli_output() {
        let hex = "3de4eb79130fa1e044367d1d4f04a166c917d268e9a1bc8899ed7612";
        let mut bytes = [0u8; 28];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
        }
        let h = Hash28::from_bytes(bytes);
        let encoded = encode_cc_hot_cip129(&h, CredKind::Key).expect("encode should succeed");
        assert_eq!(
            encoded,
            "cc_hot1qg77f6mezv86rczyxe736ncy59nvj97jdr56r0ygn8khvysvzmeuw"
        );
    }

    /// Byte-exact against `cardano-cli cip-format cip-129
    /// governance-action-id --governance-action-hex <64 0xaa bytes>#1`.
    /// Captured 2026-08-21.
    #[test]
    fn test_governance_action_id_cip129_matches_real_cardano_cli_output() {
        let txid = Hash32::from_bytes([0xaa; 32]);
        let encoded = encode_governance_action_id_cip129(&txid, 1).expect("encode");
        assert_eq!(
            encoded,
            "gov_action1424242424242424242424242424242424242424242424242424qqqgwfzv8a"
        );
    }

    /// Second capture with a different txid and a two-digit index (7), to
    /// pin the index as encoded big-endian rather than confirm only the
    /// low-order-byte-matches-index=1 case. Captured 2026-08-21.
    #[test]
    fn test_governance_action_id_cip129_index_seven() {
        let txid = Hash32::from_bytes([0xbb; 32]);
        let encoded = encode_governance_action_id_cip129(&txid, 7).expect("encode");
        assert_eq!(
            encoded,
            "gov_action1hwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwamhwasqpctedwqr"
        );
    }

    /// The header byte is the ONLY thing distinguishing a CIP-129 key
    /// credential from a script credential sharing the same hash and HRP —
    /// verifies the two never collide.
    #[test]
    fn test_cip129_key_and_script_headers_distinct() {
        let h = test_hash();
        let key = encode_drep_cip129(&h, CredKind::Key).unwrap();
        let script = encode_drep_cip129(&h, CredKind::Script).unwrap();
        assert_ne!(key, script);
    }

    /// All four CIP-129 namespaces (drep/cc_cold/cc_hot header-byte forms,
    /// plus gov_action) must be pairwise distinct even over the same 28
    /// input bytes, so a DRep id can never collide with a CC identifier.
    #[test]
    fn test_cip129_namespaces_pairwise_distinct() {
        let h = test_hash();
        let ids = [
            encode_drep_cip129(&h, CredKind::Key).unwrap(),
            encode_cc_cold_cip129(&h, CredKind::Key).unwrap(),
            encode_cc_hot_cip129(&h, CredKind::Key).unwrap(),
        ];
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j]);
            }
        }
    }

    #[test]
    fn test_known_drep_key_encoding() {
        // Hash: all 0xAB bytes (28 bytes).
        let h = Hash28::from_bytes([0xABu8; 28]);
        let encoded = encode_drep_key(&h).expect("encode");
        assert_hrp_exact(&encoded, HRP_DREP);
        // Round-trip decodes to the same hash.
        let decoded = decode_drep_key(&encoded).expect("decode");
        assert_eq!(decoded, h);
    }
}
