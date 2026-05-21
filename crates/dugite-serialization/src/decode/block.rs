//! Cardano block envelope walker and era dispatch stub.
//!
//! Cardano's hard-fork combinator wraps each block as a 2-element CBOR array:
//!
//! ```text
//! block = [era_tag :: uint, inner_block :: any]
//! ```
//!
//! The `era_tag` identifies the Cardano era and determines the structure of
//! `inner_block`. This module:
//!
//! 1. Defines [`EraTag`] — the set of Cardano era tags.
//! 2. Provides [`decode_block_envelope`] — walks the outer 2-element array,
//!    reads the era tag, and returns a zero-copy slice of the inner CBOR.
//! 3. Provides the top-level [`decode_block`] stub. Per-era implementations
//!    land in subsequent sub-PRs (M4a/b/c); this file records the dispatch
//!    structure so era files can plug in without modifying the public API.
//!
//! # Era tag values
//!
//! | Tag | Era        | Notes                                              |
//! |-----|------------|----------------------------------------------------|
//! | 0   | Byron main | Full mainnet/preprod Byron blocks                  |
//! | 1   | Byron EBB  | Epoch boundary blocks (no body, just boundary hash)|
//! | 2   | Shelley    | First Praos era                                    |
//! | 3   | Allegra    | Multi-asset scripts added                          |
//! | 4   | Mary       | Native multi-asset                                 |
//! | 5   | Alonzo     | Plutus V1                                          |
//! | 6   | Babbage    | Plutus V2 + inline datums + reference inputs       |
//! | 7   | Conway     | Governance (CIP-1694), Plutus V3                   |
//! | 8   | Dijkstra   | Peras + new TxBody keys 14/23/25/26                |

use crate::decode::era_allegra;
use crate::decode::era_alonzo;
use crate::decode::era_babbage;
use crate::decode::era_byron;
use crate::decode::era_conway;
use crate::decode::era_mary;
use crate::decode::era_shelley;
use crate::decode::reader::Reader;
use crate::error::SerializationError;
use dugite_primitives::block::Block;

/// Cardano era tags as used in the hard-fork combinator block envelope.
///
/// The numeric values match the on-wire CBOR uint values exactly. Unknown
/// future era tags are captured in the `Unknown` variant so that the decoder
/// fails loudly rather than silently decoding garbage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EraTag {
    /// Era tag 0: Byron main-chain block.
    ByronMain,
    /// Era tag 1: Byron epoch boundary block (EBB).
    ///
    /// EBBs contain only a boundary header with the epoch nonce; they have no
    /// transaction body and no Praos fields.
    ByronEbb,
    /// Era tag 2: Shelley.
    Shelley,
    /// Era tag 3: Allegra (multi-asset scripts).
    Allegra,
    /// Era tag 4: Mary (native multi-asset).
    Mary,
    /// Era tag 5: Alonzo (Plutus V1).
    Alonzo,
    /// Era tag 6: Babbage (Plutus V2, inline datums, reference inputs).
    Babbage,
    /// Era tag 7: Conway (CIP-1694 governance, Plutus V3).
    Conway,
    /// Era tag 8: Dijkstra (Peras certificates, new TxBody keys).
    ///
    /// Added natively here to avoid the Conway-rewrite shim in the pallas-based
    /// decoder. Once the in-house decoder is wired to this era, remove the shim
    /// in `multi_era.rs`.
    Dijkstra,
    /// An unrecognised era tag, preserved for forward compatibility.
    Unknown(u64),
}

impl EraTag {
    /// Parse a raw uint CBOR value into an `EraTag`.
    pub fn from_u64(n: u64) -> Self {
        match n {
            0 => EraTag::ByronMain,
            1 => EraTag::ByronEbb,
            2 => EraTag::Shelley,
            3 => EraTag::Allegra,
            4 => EraTag::Mary,
            5 => EraTag::Alonzo,
            6 => EraTag::Babbage,
            7 => EraTag::Conway,
            8 => EraTag::Dijkstra,
            n => EraTag::Unknown(n),
        }
    }

    /// Return the raw u64 tag value.
    pub fn as_u64(self) -> u64 {
        match self {
            EraTag::ByronMain => 0,
            EraTag::ByronEbb => 1,
            EraTag::Shelley => 2,
            EraTag::Allegra => 3,
            EraTag::Mary => 4,
            EraTag::Alonzo => 5,
            EraTag::Babbage => 6,
            EraTag::Conway => 7,
            EraTag::Dijkstra => 8,
            EraTag::Unknown(n) => n,
        }
    }

    /// Return `true` for Byron era tags (both main and EBB).
    pub fn is_byron(self) -> bool {
        matches!(self, EraTag::ByronMain | EraTag::ByronEbb)
    }

    /// Return `true` if this era uses the Shelley/Praos block format.
    ///
    /// All post-Byron eras share the same outer block structure (Praos header +
    /// body tuple) with per-era differences in the transaction body fields.
    pub fn is_shelley_family(self) -> bool {
        !self.is_byron() && !matches!(self, EraTag::Unknown(_))
    }
}

impl std::fmt::Display for EraTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EraTag::ByronMain => write!(f, "ByronMain(0)"),
            EraTag::ByronEbb => write!(f, "ByronEbb(1)"),
            EraTag::Shelley => write!(f, "Shelley(2)"),
            EraTag::Allegra => write!(f, "Allegra(3)"),
            EraTag::Mary => write!(f, "Mary(4)"),
            EraTag::Alonzo => write!(f, "Alonzo(5)"),
            EraTag::Babbage => write!(f, "Babbage(6)"),
            EraTag::Conway => write!(f, "Conway(7)"),
            EraTag::Dijkstra => write!(f, "Dijkstra(8)"),
            EraTag::Unknown(n) => write!(f, "Unknown({n})"),
        }
    }
}

/// Walk the outer block envelope and return the era tag and inner CBOR slice.
///
/// The Cardano hard-fork combinator wraps every block as:
///
/// ```text
/// [era_tag :: uint, inner_block :: any]
/// ```
///
/// This function:
/// 1. Reads the 2-element outer array header.
/// 2. Reads the era tag uint.
/// 3. Notes the current position (start of `inner_block`).
/// 4. Skips the inner value to advance to its end.
/// 5. Returns a zero-copy slice `&cbor[inner_start..inner_end]`.
///
/// The returned slice is borrowed from `cbor` — no allocation. Era-specific
/// decoders receive this slice as their input.
///
/// # Errors
///
/// Returns an error if:
/// - The outer structure is not a 2-element CBOR array.
/// - The era tag is not a valid CBOR uint.
/// - The inner value is malformed (skip fails).
pub fn decode_block_envelope<'b>(
    r: &mut Reader<'b>,
) -> Result<(EraTag, &'b [u8]), SerializationError> {
    // Expect a 2-element definite-length array.
    let arr_len = r.read_array_header()?;
    match arr_len {
        Some(2) => {}
        Some(n) => {
            return Err(SerializationError::CborDecode(format!(
                "block envelope: expected array(2), got array({n})"
            )));
        }
        None => {
            return Err(SerializationError::CborDecode(
                "block envelope: expected definite-length array(2), got indefinite".into(),
            ));
        }
    }

    // Read the era tag.
    let era_raw = r.read_uint()?;
    let era_tag = EraTag::from_u64(era_raw);

    // Capture the inner block CBOR.
    let inner_start = r.position();
    r.skip()?; // advances past the entire inner value
    let inner_cbor = r.slice_from(inner_start);

    Ok((era_tag, inner_cbor))
}

/// Decode a multi-era block from raw CBOR.
///
/// This is the top-level dispatch point for the in-house decoder. It walks the
/// block envelope via [`decode_block_envelope`] and routes to the per-era decoder.
///
/// `byron_epoch_length`: Byron-era epoch slot count. Pass `0` for mainnet
/// (uses the 21600 slot-per-epoch mainnet GenesisValues formula). Pass the
/// network-specific value for preview/preprod/custom networks.
///
/// `minimal`: if `true`, witness sets are skipped for faster replay.
///
/// # Status
///
/// - M4a: Byron (eras 0/1) and Shelley (era 2) are implemented in-house.
/// - M4b: Allegra (era 3), Mary (era 4), Alonzo (era 5), Babbage (era 6) implemented.
/// - M4c: Conway (era 7) and Dijkstra (era 8) implemented.
///
/// # Errors
///
/// Returns `SerializationError::CborDecode` if the outer CBOR envelope is malformed.
pub fn decode_block(
    cbor: &[u8],
    byron_epoch_length: u64,
    minimal: bool,
) -> Result<Block, SerializationError> {
    let mut r = Reader::new(cbor);
    let (era_tag, inner_cbor) = decode_block_envelope(&mut r)?;

    match era_tag {
        EraTag::ByronMain => era_byron::decode_byron_main_block(inner_cbor, byron_epoch_length),
        EraTag::ByronEbb => era_byron::decode_byron_ebb_block(inner_cbor, byron_epoch_length),
        EraTag::Shelley => {
            if minimal {
                era_shelley::decode_shelley_block_minimal(inner_cbor)
            } else {
                era_shelley::decode_shelley_block(inner_cbor)
            }
        }
        EraTag::Allegra => {
            if minimal {
                era_allegra::decode_allegra_block_minimal(inner_cbor)
            } else {
                era_allegra::decode_allegra_block(inner_cbor)
            }
        }
        EraTag::Mary => {
            if minimal {
                era_mary::decode_mary_block_minimal(inner_cbor)
            } else {
                era_mary::decode_mary_block(inner_cbor)
            }
        }
        EraTag::Alonzo => {
            if minimal {
                era_alonzo::decode_alonzo_block_minimal(inner_cbor)
            } else {
                era_alonzo::decode_alonzo_block(inner_cbor)
            }
        }
        EraTag::Babbage => {
            if minimal {
                era_babbage::decode_babbage_block_minimal(inner_cbor)
            } else {
                era_babbage::decode_babbage_block(inner_cbor)
            }
        }
        EraTag::Conway => {
            if minimal {
                era_conway::decode_conway_block_minimal(inner_cbor)
            } else {
                era_conway::decode_conway_block(inner_cbor)
            }
        }
        EraTag::Dijkstra => {
            if minimal {
                era_conway::decode_dijkstra_block_minimal(inner_cbor)
            } else {
                era_conway::decode_dijkstra_block(inner_cbor)
            }
        }
        EraTag::Unknown(tag) => Err(SerializationError::CborDecode(format!(
            "decode_block: unknown era tag {tag}"
        ))),
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // EraTag round-trips
    // -----------------------------------------------------------------------

    #[test]
    fn era_tag_from_u64_all_known() {
        let pairs: &[(u64, EraTag)] = &[
            (0, EraTag::ByronMain),
            (1, EraTag::ByronEbb),
            (2, EraTag::Shelley),
            (3, EraTag::Allegra),
            (4, EraTag::Mary),
            (5, EraTag::Alonzo),
            (6, EraTag::Babbage),
            (7, EraTag::Conway),
            (8, EraTag::Dijkstra),
        ];
        for &(n, ref expected) in pairs {
            let tag = EraTag::from_u64(n);
            assert_eq!(&tag, expected, "EraTag::from_u64({n})");
            assert_eq!(tag.as_u64(), n, "EraTag::as_u64() round-trip for {n}");
        }
    }

    #[test]
    fn era_tag_unknown() {
        let tag = EraTag::from_u64(42);
        assert_eq!(tag, EraTag::Unknown(42));
        assert_eq!(tag.as_u64(), 42);
    }

    #[test]
    fn era_tag_is_byron() {
        assert!(EraTag::ByronMain.is_byron());
        assert!(EraTag::ByronEbb.is_byron());
        assert!(!EraTag::Shelley.is_byron());
        assert!(!EraTag::Conway.is_byron());
        assert!(!EraTag::Dijkstra.is_byron());
    }

    #[test]
    fn era_tag_is_shelley_family() {
        assert!(!EraTag::ByronMain.is_shelley_family());
        assert!(!EraTag::ByronEbb.is_shelley_family());
        assert!(EraTag::Shelley.is_shelley_family());
        assert!(EraTag::Babbage.is_shelley_family());
        assert!(EraTag::Conway.is_shelley_family());
        assert!(EraTag::Dijkstra.is_shelley_family());
        assert!(!EraTag::Unknown(99).is_shelley_family());
    }

    #[test]
    fn era_tag_display() {
        assert_eq!(EraTag::Conway.to_string(), "Conway(7)");
        assert_eq!(EraTag::Dijkstra.to_string(), "Dijkstra(8)");
        assert_eq!(EraTag::Unknown(99).to_string(), "Unknown(99)");
    }

    // -----------------------------------------------------------------------
    // Helpers for building minimal envelope CBOR
    // -----------------------------------------------------------------------

    /// Build `[era_tag_uint, inner_cbor_bytes]` as a 2-element array.
    fn make_envelope(era_tag: u64, inner: &[u8]) -> Vec<u8> {
        let mut v = vec![0x82u8]; // array(2)
                                  // CBOR-encode era_tag as a uint, picking the smallest representation.
        if era_tag < 24 {
            v.push(era_tag as u8);
        } else if era_tag < 256 {
            v.push(0x18);
            v.push(era_tag as u8);
        } else if era_tag < 65536 {
            v.push(0x19);
            v.extend_from_slice(&(era_tag as u16).to_be_bytes());
        } else {
            v.push(0x1b);
            v.extend_from_slice(&era_tag.to_be_bytes());
        }
        v.extend_from_slice(inner);
        v
    }

    /// Minimal "inner block" — just a definite-length array with one uint.
    /// This is structurally valid CBOR; decoders ignore the content.
    fn minimal_inner(tag: u64) -> Vec<u8> {
        vec![0x81, tag as u8] // array(1)[tag]
    }

    // -----------------------------------------------------------------------
    // decode_block_envelope
    // -----------------------------------------------------------------------

    #[test]
    fn envelope_shelley() {
        let inner = minimal_inner(2);
        let cbor = make_envelope(2, &inner);
        let mut r = Reader::new(&cbor);
        let (era, inner_slice) = decode_block_envelope(&mut r).unwrap();
        assert_eq!(era, EraTag::Shelley);
        assert_eq!(inner_slice, inner.as_slice());
    }

    #[test]
    fn envelope_conway() {
        let inner = minimal_inner(7);
        let cbor = make_envelope(7, &inner);
        let mut r = Reader::new(&cbor);
        let (era, inner_slice) = decode_block_envelope(&mut r).unwrap();
        assert_eq!(era, EraTag::Conway);
        assert_eq!(inner_slice, inner.as_slice());
    }

    #[test]
    fn envelope_dijkstra() {
        let inner = minimal_inner(8);
        let cbor = make_envelope(8, &inner);
        let mut r = Reader::new(&cbor);
        let (era, inner_slice) = decode_block_envelope(&mut r).unwrap();
        assert_eq!(era, EraTag::Dijkstra);
        assert_eq!(inner_slice, inner.as_slice());
    }

    #[test]
    fn envelope_unknown_tag_returns_unknown_variant() {
        let inner = minimal_inner(0);
        let cbor = make_envelope(99, &inner);
        let mut r = Reader::new(&cbor);
        let (era, _) = decode_block_envelope(&mut r).unwrap();
        assert_eq!(era, EraTag::Unknown(99));
    }

    #[test]
    fn envelope_byron_main() {
        let inner = minimal_inner(0);
        let cbor = make_envelope(0, &inner);
        let mut r = Reader::new(&cbor);
        let (era, inner_slice) = decode_block_envelope(&mut r).unwrap();
        assert_eq!(era, EraTag::ByronMain);
        assert_eq!(inner_slice, inner.as_slice());
    }

    #[test]
    fn envelope_wrong_outer_length_rejected() {
        // array(3)[era, inner, extra]
        let mut v = vec![0x83u8]; // array(3)
        v.push(2u8); // era = Shelley
        v.extend(minimal_inner(2));
        v.push(0u8); // extra element
        let mut r = Reader::new(&v);
        let err = decode_block_envelope(&mut r).unwrap_err();
        assert!(matches!(err, SerializationError::CborDecode(_)));
        assert!(format!("{err}").contains("array(3)"));
    }

    #[test]
    fn envelope_indefinite_outer_rejected() {
        // 0x9f = indefinite array
        let v = vec![0x9f, 0x02, 0x81, 0x00, 0xff];
        let mut r = Reader::new(&v);
        let err = decode_block_envelope(&mut r).unwrap_err();
        assert!(matches!(err, SerializationError::CborDecode(_)));
        assert!(format!("{err}").contains("indefinite"));
    }

    #[test]
    fn envelope_empty_inner_slot_size() {
        // Verify the inner_cbor slice covers exactly the inner element.
        let inner_bytes = vec![0x40u8]; // bytes([]) — one byte
        let cbor = make_envelope(6, &inner_bytes);
        let mut r = Reader::new(&cbor);
        let (era, inner_slice) = decode_block_envelope(&mut r).unwrap();
        assert_eq!(era, EraTag::Babbage);
        assert_eq!(inner_slice, inner_bytes.as_slice());
    }

    #[test]
    fn envelope_reader_exhausted_after_decode() {
        let inner = minimal_inner(5);
        let cbor = make_envelope(5, &inner);
        let total = cbor.len();
        let mut r = Reader::new(&cbor);
        decode_block_envelope(&mut r).unwrap();
        assert_eq!(r.position(), total);
    }

    // -----------------------------------------------------------------------
    // decode_block — unknown-tag error path and Conway/Dijkstra dispatch.
    // -----------------------------------------------------------------------

    #[test]
    fn decode_block_unknown_era_returns_error() {
        let inner = minimal_inner(0);
        let cbor = make_envelope(99, &inner);
        let err = decode_block(&cbor, 0, false).unwrap_err();
        assert!(matches!(err, SerializationError::CborDecode(_)));
        assert!(format!("{err}").contains("99"));
    }

    /// Conway (era 7) is now implemented in-house; a minimal (invalid) block
    /// must return a `CborDecode` error, NOT an `unimplemented!()` panic.
    #[test]
    fn decode_block_conway_returns_decode_error_not_panic() {
        let inner = minimal_inner(7);
        let cbor = make_envelope(7, &inner);
        let result = decode_block(&cbor, 0, false);
        assert!(
            result.is_err(),
            "minimal synthetic Conway block must fail to decode"
        );
        assert!(
            !matches!(result, Err(SerializationError::CborDecode(ref m)) if m.contains("not yet implemented")),
            "must not be an unimplemented!() panic disguised as an error"
        );
    }

    /// Dijkstra (era 8) is now implemented in-house (via the Conway decoder);
    /// a minimal (invalid) block must return a `CborDecode` error, NOT a panic.
    #[test]
    fn decode_block_dijkstra_returns_decode_error_not_panic() {
        let inner = minimal_inner(8);
        let cbor = make_envelope(8, &inner);
        let result = decode_block(&cbor, 0, false);
        assert!(
            result.is_err(),
            "minimal synthetic Dijkstra block must fail to decode"
        );
    }
}
