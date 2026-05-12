//! Variable-length CBOR encode / decode helpers for `EraParams` and `Bound`
//! per issue [#459](https://github.com/dugite-fps/dugite/issues/459).
//!
//! ## Background
//!
//! Haskell `ouroboros-consensus` 1.0.0.0 extended the HardFork History
//! `EraParams` and `Bound` types with optional **Peras** fields, encoded by
//! varying the surrounding CBOR list length rather than wrapping in
//! `Maybe`/`null`:
//!
//! | Type        | Pre-Peras                                                     | Peras-enabled                                      |
//! |-------------|---------------------------------------------------------------|----------------------------------------------------|
//! | `EraParams` | `array(4)[epoch_size, slot_length_ms, safe_zone, gen_window]` | `array(5)[…, peras_round_length]`                  |
//! | `Bound`     | `array(3)[time_pico, slot, epoch]`                            | `array(4)[…, peras_round]`                         |
//!
//! For current mainnet and testnets (Peras disabled), Haskell still emits the
//! 4 / 3-element forms — dugite is wire-compatible today. The helpers below
//! prepare the codebase for Peras activation:
//!
//! * `encode_era_params` / `encode_bound` switch to the longer form **only**
//!   when an `Option<u64>` is supplied (`Some` ⇒ Peras-aware).
//! * `decode_era_params` / `decode_bound` accept **either** length and return
//!   the optional Peras field, so dugite will keep parsing v11+ era-history
//!   payloads if and when Peras activates.
//!
//! These helpers are intentionally independent of
//! [`crate::era_history::EraParams`] / [`crate::era_history::Bound`] so they
//! can be used at the wire boundary (N2C `GetEraSummaries` /
//! `GetInterpreter`) without forcing those structs to carry new fields. The
//! callers in `dugite-node` thread an `Option<u64>` from configuration.
//!
//! ## References
//! * `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/HardFork/History/EraParams.hs`
//! * `ouroboros-consensus/src/ouroboros-consensus/Ouroboros/Consensus/HardFork/History/Summary.hs`

use minicbor::{Decoder, Encoder};

/// Plain wire representation of Haskell's `EraParams` (issue #459).
///
/// The `peras_round_length` field is `None` when the on-wire array length is
/// 4 (pre-Peras) and `Some(_)` when the length is 5 (Peras-enabled).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EraParamsWire {
    pub epoch_size: u64,
    pub slot_length_ms: u64,
    pub safe_zone: u64,
    pub genesis_window: u64,
    /// Peras round length (slots). `None` ⇒ encode as 4-element array.
    pub peras_round_length: Option<u64>,
}

/// Plain wire representation of Haskell's `Bound` (issue #459).
///
/// The `peras_round` field is `None` when the on-wire array length is 3
/// (pre-Peras) and `Some(_)` when the length is 4 (Peras-enabled).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoundWire {
    /// Picoseconds relative to system start.
    pub time_pico: u128,
    pub slot: u64,
    pub epoch: u64,
    /// Peras round number. `None` ⇒ encode as 3-element array.
    pub peras_round: Option<u64>,
}

/// Errors raised by the variable-length decoders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerasWireError {
    /// CBOR list length is not in the accepted set.
    BadArrayLen { got: u64, accepted: &'static [u64] },
    /// Underlying minicbor decode error.
    Cbor(String),
}

impl std::fmt::Display for PerasWireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PerasWireError::BadArrayLen { got, accepted } => write!(
                f,
                "unexpected CBOR array length {got}, accepted={accepted:?}"
            ),
            PerasWireError::Cbor(msg) => write!(f, "CBOR decode error: {msg}"),
        }
    }
}

impl std::error::Error for PerasWireError {}

impl From<minicbor::decode::Error> for PerasWireError {
    fn from(e: minicbor::decode::Error) -> Self {
        PerasWireError::Cbor(e.to_string())
    }
}

/// Encode an `EraParams` as a definite-length CBOR array.
///
/// Length is **4** when `peras_round_length` is `None`, **5** when `Some`.
/// This matches the Haskell `Serialise` instance under both Peras-disabled
/// and Peras-enabled `ouroboros-consensus` 1.0.0.0.
pub fn encode_era_params(enc: &mut Encoder<&mut Vec<u8>>, p: &EraParamsWire) {
    let len: u64 = if p.peras_round_length.is_some() { 5 } else { 4 };
    enc.array(len).ok();
    enc.u64(p.epoch_size).ok();
    enc.u64(p.slot_length_ms).ok();
    enc.u64(p.safe_zone).ok();
    enc.u64(p.genesis_window).ok();
    if let Some(r) = p.peras_round_length {
        enc.u64(r).ok();
    }
}

/// Encode a `Bound` as a definite-length CBOR array.
///
/// Length is **3** when `peras_round` is `None`, **4** when `Some`. Picosecond
/// time values that exceed `u64::MAX` are encoded as CBOR tag-2 bignums to
/// match Haskell's `Serialise Pico` instance.
pub fn encode_bound(enc: &mut Encoder<&mut Vec<u8>>, b: &BoundWire) {
    let len: u64 = if b.peras_round.is_some() { 4 } else { 3 };
    enc.array(len).ok();
    encode_pico(enc, b.time_pico);
    enc.u64(b.slot).ok();
    enc.u64(b.epoch).ok();
    if let Some(r) = b.peras_round {
        enc.u64(r).ok();
    }
}

/// Decode an `EraParams` from either the 4-element (pre-Peras) or 5-element
/// (Peras-enabled) Haskell CBOR wire form.
///
/// Returns `Err(PerasWireError::BadArrayLen)` when the array length is
/// neither 4 nor 5. Indefinite-length arrays are accepted as well: the
/// decoder reads up to 5 entries before expecting the break code.
pub fn decode_era_params(dec: &mut Decoder<'_>) -> Result<EraParamsWire, PerasWireError> {
    let len_opt = dec.array()?;
    match len_opt {
        Some(len) if len == 4 || len == 5 => {
            let epoch_size = dec.u64()?;
            let slot_length_ms = dec.u64()?;
            let safe_zone = dec.u64()?;
            let genesis_window = dec.u64()?;
            let peras_round_length = if len == 5 { Some(dec.u64()?) } else { None };
            Ok(EraParamsWire {
                epoch_size,
                slot_length_ms,
                safe_zone,
                genesis_window,
                peras_round_length,
            })
        }
        Some(other) => Err(PerasWireError::BadArrayLen {
            got: other,
            accepted: &[4, 5],
        }),
        None => {
            // Indefinite-length array form (`0x9f ... 0xff`). Read 4 mandatory
            // elements, then optionally one more before the break.
            let epoch_size = dec.u64()?;
            let slot_length_ms = dec.u64()?;
            let safe_zone = dec.u64()?;
            let genesis_window = dec.u64()?;
            let peras_round_length = read_optional_break_u64(dec)?;
            Ok(EraParamsWire {
                epoch_size,
                slot_length_ms,
                safe_zone,
                genesis_window,
                peras_round_length,
            })
        }
    }
}

/// Decode a `Bound` from either the 3-element (pre-Peras) or 4-element
/// (Peras-enabled) Haskell CBOR wire form.
///
/// Picosecond time values are accepted as either a CBOR unsigned integer
/// (`u64`-fits) or a CBOR tag-2 positive bignum (full `u128`), matching the
/// encoder. Indefinite-length array form is also supported.
pub fn decode_bound(dec: &mut Decoder<'_>) -> Result<BoundWire, PerasWireError> {
    let len_opt = dec.array()?;
    match len_opt {
        Some(len) if len == 3 || len == 4 => {
            let time_pico = decode_pico(dec)?;
            let slot = dec.u64()?;
            let epoch = dec.u64()?;
            let peras_round = if len == 4 { Some(dec.u64()?) } else { None };
            Ok(BoundWire {
                time_pico,
                slot,
                epoch,
                peras_round,
            })
        }
        Some(other) => Err(PerasWireError::BadArrayLen {
            got: other,
            accepted: &[3, 4],
        }),
        None => {
            let time_pico = decode_pico(dec)?;
            let slot = dec.u64()?;
            let epoch = dec.u64()?;
            let peras_round = read_optional_break_u64(dec)?;
            Ok(BoundWire {
                time_pico,
                slot,
                epoch,
                peras_round,
            })
        }
    }
}

/// Encode a u128 picosecond timestamp using the Haskell `Fixed E12` (Pico)
/// CBOR rule: plain unsigned int when it fits in u64, otherwise CBOR tag 2
/// (positive bignum). Duplicates the helper in `dugite-node` to keep the
/// consensus crate dependency-free of the node binary.
fn encode_pico(enc: &mut Encoder<&mut Vec<u8>>, value: u128) {
    if value <= u64::MAX as u128 {
        enc.u64(value as u64).ok();
    } else {
        enc.tag(minicbor::data::Tag::new(2)).ok();
        let bytes = value.to_be_bytes();
        let start = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
        enc.bytes(&bytes[start..]).ok();
    }
}

/// Decode a picosecond timestamp encoded by [`encode_pico`].
fn decode_pico(dec: &mut Decoder<'_>) -> Result<u128, PerasWireError> {
    let pos = dec.position();
    // Try plain u64 first.
    if let Ok(n) = dec.u64() {
        return Ok(n as u128);
    }
    dec.set_position(pos);
    // Tag-2 positive bignum.
    let tag = dec.tag()?;
    if tag != minicbor::data::Tag::new(2) {
        return Err(PerasWireError::Cbor(format!(
            "expected CBOR tag 2 (bignum) or u64 for pico, got tag {tag:?}"
        )));
    }
    let bytes = dec.bytes()?;
    if bytes.len() > 16 {
        return Err(PerasWireError::Cbor(format!(
            "pico bignum too large for u128: {} bytes",
            bytes.len()
        )));
    }
    let mut buf = [0u8; 16];
    buf[16 - bytes.len()..].copy_from_slice(bytes);
    Ok(u128::from_be_bytes(buf))
}

/// Helper for indefinite-length array decoding: peek at the next byte; if it
/// is the CBOR break (0xff) consume it and return `None`, otherwise read one
/// more `u64` and **then** consume the break.
fn read_optional_break_u64(dec: &mut Decoder<'_>) -> Result<Option<u64>, PerasWireError> {
    // minicbor exposes `.datatype()` to peek without advancing.
    let dt = dec.datatype()?;
    if dt == minicbor::data::Type::Break {
        // Consume the break marker.
        let _ = dec.skip();
        return Ok(None);
    }
    let v = dec.u64()?;
    // Expect break next.
    let dt2 = dec.datatype()?;
    if dt2 == minicbor::data::Type::Break {
        let _ = dec.skip();
    }
    Ok(Some(v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc_bytes<F: FnOnce(&mut Encoder<&mut Vec<u8>>)>(f: F) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            f(&mut enc);
        }
        buf
    }

    // -----------------------------------------------------------------
    // EraParams
    // -----------------------------------------------------------------

    #[test]
    fn era_params_encode_definite_pre_peras() {
        let p = EraParamsWire {
            epoch_size: 432_000,
            slot_length_ms: 1_000,
            safe_zone: 129_600,
            genesis_window: 4_320,
            peras_round_length: None,
        };
        let bytes = enc_bytes(|e| encode_era_params(e, &p));
        // First byte must be CBOR array(4) = 0x84.
        assert_eq!(
            bytes[0], 0x84,
            "pre-Peras EraParams must emit CBOR array(4), got 0x{:02x}",
            bytes[0]
        );
    }

    #[test]
    fn era_params_encode_definite_peras_enabled() {
        let p = EraParamsWire {
            epoch_size: 432_000,
            slot_length_ms: 1_000,
            safe_zone: 129_600,
            genesis_window: 4_320,
            peras_round_length: Some(900),
        };
        let bytes = enc_bytes(|e| encode_era_params(e, &p));
        // First byte must be CBOR array(5) = 0x85.
        assert_eq!(
            bytes[0], 0x85,
            "Peras-enabled EraParams must emit CBOR array(5), got 0x{:02x}",
            bytes[0]
        );
    }

    #[test]
    fn era_params_roundtrip_pre_peras() {
        let p = EraParamsWire {
            epoch_size: 21_600,
            slot_length_ms: 20_000,
            safe_zone: 4_320,
            genesis_window: 4_320,
            peras_round_length: None,
        };
        let bytes = enc_bytes(|e| encode_era_params(e, &p));
        let mut dec = Decoder::new(&bytes);
        let got = decode_era_params(&mut dec).expect("decode pre-Peras EraParams");
        assert_eq!(got, p);
    }

    #[test]
    fn era_params_roundtrip_peras() {
        let p = EraParamsWire {
            epoch_size: 432_000,
            slot_length_ms: 1_000,
            safe_zone: 129_600,
            genesis_window: 4_320,
            peras_round_length: Some(2_160),
        };
        let bytes = enc_bytes(|e| encode_era_params(e, &p));
        let mut dec = Decoder::new(&bytes);
        let got = decode_era_params(&mut dec).expect("decode Peras EraParams");
        assert_eq!(got, p);
    }

    #[test]
    fn era_params_decode_accepts_indefinite_pre_peras() {
        // 0x9f <4 u64s> 0xff
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.begin_array().ok();
            enc.u64(100).ok();
            enc.u64(1_000).ok();
            enc.u64(2_000).ok();
            enc.u64(3_000).ok();
            enc.end().ok();
        }
        let mut dec = Decoder::new(&buf);
        let got = decode_era_params(&mut dec).expect("indefinite-length 4-elem");
        assert_eq!(
            got,
            EraParamsWire {
                epoch_size: 100,
                slot_length_ms: 1_000,
                safe_zone: 2_000,
                genesis_window: 3_000,
                peras_round_length: None,
            }
        );
    }

    #[test]
    fn era_params_decode_accepts_indefinite_peras() {
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.begin_array().ok();
            enc.u64(100).ok();
            enc.u64(1_000).ok();
            enc.u64(2_000).ok();
            enc.u64(3_000).ok();
            enc.u64(4_321).ok();
            enc.end().ok();
        }
        let mut dec = Decoder::new(&buf);
        let got = decode_era_params(&mut dec).expect("indefinite-length 5-elem");
        assert_eq!(got.peras_round_length, Some(4_321));
    }

    #[test]
    fn era_params_decode_rejects_bad_length() {
        // array(3) — too short.
        let bytes = enc_bytes(|e| {
            e.array(3).ok();
            e.u64(1).ok();
            e.u64(2).ok();
            e.u64(3).ok();
        });
        let mut dec = Decoder::new(&bytes);
        let err = decode_era_params(&mut dec).expect_err("must reject array(3)");
        match err {
            PerasWireError::BadArrayLen { got: 3, .. } => {}
            other => panic!("expected BadArrayLen{{got:3}}, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Bound
    // -----------------------------------------------------------------

    #[test]
    fn bound_encode_definite_pre_peras() {
        let b = BoundWire {
            time_pico: 1_000_000_000_000, // 1 second
            slot: 100,
            epoch: 0,
            peras_round: None,
        };
        let bytes = enc_bytes(|e| encode_bound(e, &b));
        assert_eq!(
            bytes[0], 0x83,
            "pre-Peras Bound must emit CBOR array(3), got 0x{:02x}",
            bytes[0]
        );
    }

    #[test]
    fn bound_encode_definite_peras_enabled() {
        let b = BoundWire {
            time_pico: 0,
            slot: 0,
            epoch: 0,
            peras_round: Some(1),
        };
        let bytes = enc_bytes(|e| encode_bound(e, &b));
        assert_eq!(
            bytes[0], 0x84,
            "Peras-enabled Bound must emit CBOR array(4), got 0x{:02x}",
            bytes[0]
        );
    }

    #[test]
    fn bound_roundtrip_pre_peras_small_time() {
        let b = BoundWire {
            time_pico: 1_234_567_890_123, // fits in u64
            slot: 4_492_800,
            epoch: 208,
            peras_round: None,
        };
        let bytes = enc_bytes(|e| encode_bound(e, &b));
        let mut dec = Decoder::new(&bytes);
        let got = decode_bound(&mut dec).expect("decode pre-Peras Bound");
        assert_eq!(got, b);
    }

    #[test]
    fn bound_roundtrip_pre_peras_bignum_time() {
        // Mainnet Byron-end pico (~9e19) exceeds u64::MAX → tag-2 bignum.
        let big = 4_492_800u128 * 20_000u128 * 1_000_000_000u128; // ~9e19
        assert!(big > u64::MAX as u128);
        let b = BoundWire {
            time_pico: big,
            slot: 4_492_800,
            epoch: 208,
            peras_round: None,
        };
        let bytes = enc_bytes(|e| encode_bound(e, &b));
        let mut dec = Decoder::new(&bytes);
        let got = decode_bound(&mut dec).expect("decode bignum Bound");
        assert_eq!(got, b);
    }

    #[test]
    fn bound_roundtrip_peras() {
        let b = BoundWire {
            time_pico: 1_000,
            slot: 50,
            epoch: 5,
            peras_round: Some(42),
        };
        let bytes = enc_bytes(|e| encode_bound(e, &b));
        let mut dec = Decoder::new(&bytes);
        let got = decode_bound(&mut dec).expect("decode Peras Bound");
        assert_eq!(got, b);
    }

    #[test]
    fn bound_decode_accepts_indefinite_pre_peras() {
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.begin_array().ok();
            enc.u64(1_000).ok();
            enc.u64(50).ok();
            enc.u64(5).ok();
            enc.end().ok();
        }
        let mut dec = Decoder::new(&buf);
        let got = decode_bound(&mut dec).expect("indefinite-length 3-elem");
        assert_eq!(
            got,
            BoundWire {
                time_pico: 1_000,
                slot: 50,
                epoch: 5,
                peras_round: None,
            }
        );
    }

    #[test]
    fn bound_decode_accepts_indefinite_peras() {
        let mut buf = Vec::new();
        {
            let mut enc = Encoder::new(&mut buf);
            enc.begin_array().ok();
            enc.u64(1_000).ok();
            enc.u64(50).ok();
            enc.u64(5).ok();
            enc.u64(99).ok();
            enc.end().ok();
        }
        let mut dec = Decoder::new(&buf);
        let got = decode_bound(&mut dec).expect("indefinite-length 4-elem");
        assert_eq!(got.peras_round, Some(99));
    }

    #[test]
    fn bound_decode_rejects_bad_length() {
        let bytes = enc_bytes(|e| {
            e.array(2).ok();
            e.u64(1).ok();
            e.u64(2).ok();
        });
        let mut dec = Decoder::new(&bytes);
        let err = decode_bound(&mut dec).expect_err("must reject array(2)");
        match err {
            PerasWireError::BadArrayLen { got: 2, .. } => {}
            other => panic!("expected BadArrayLen{{got:2}}, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------
    // Cross-decoder sanity: a pre-Peras encoder output decodes cleanly
    // when the decoder is the Peras-aware one (forward compatibility).
    // -----------------------------------------------------------------

    #[test]
    fn cross_decoder_pre_peras_to_peras_aware() {
        // Encode using the pre-Peras (None) form, decode with the same
        // tolerant decoder — peras fields must be None.
        let p = EraParamsWire {
            epoch_size: 432_000,
            slot_length_ms: 1_000,
            safe_zone: 129_600,
            genesis_window: 4_320,
            peras_round_length: None,
        };
        let bytes = enc_bytes(|e| encode_era_params(e, &p));
        let mut dec = Decoder::new(&bytes);
        let got = decode_era_params(&mut dec).unwrap();
        assert!(got.peras_round_length.is_none());

        let b = BoundWire {
            time_pico: 0,
            slot: 0,
            epoch: 0,
            peras_round: None,
        };
        let bytes = enc_bytes(|e| encode_bound(e, &b));
        let mut dec = Decoder::new(&bytes);
        let got = decode_bound(&mut dec).unwrap();
        assert!(got.peras_round.is_none());
    }
}
