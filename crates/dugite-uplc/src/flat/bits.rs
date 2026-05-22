//! Bit-level I/O primitives for the flat encoding.
//!
//! `BitReader` and `BitWriter` are the foundation of the flat
//! UPLC wire format. The format packs values bit-by-bit, MSB-first,
//! with byte alignment only at "filler" boundaries (byte-string
//! payloads). All of the higher-level codec functions (naturals,
//! integers, byte strings, term tags, constant tags) sit on top of
//! these two types.
//!
//! Defensive properties:
//!
//! - `BitReader::read_bits(n)` always checks `ensure_bits(n)` first;
//!   reading past end of input returns `FlatDecode("unexpected end of input")`.
//! - `BitReader::read_natural()` rejects varints whose accumulated
//!   shift would exceed `u64::BITS` (the maximum width we plumb
//!   through the codec) — no silent wraparound, no debug-mode panic
//!   from shift overflow.
//! - No `unwrap`/`expect`/`panic!` reachable from any input byte.

use crate::UplcError;

use super::FlatResult;

/// Bit-level reader over a borrowed byte buffer. Reads are MSB-first.
#[derive(Debug)]
pub struct BitReader<'b> {
    bytes: &'b [u8],
    /// Total bits consumed from the start of `bytes`. The current
    /// byte is `bit_pos / 8`; the bit within that byte is `bit_pos % 8`
    /// counting from the MSB (i.e. bit 0 in byte 0 is the most
    /// significant bit of `bytes[0]`).
    bit_pos: usize,
}

impl<'b> BitReader<'b> {
    /// Construct a reader at the start of `bytes`.
    pub fn new(bytes: &'b [u8]) -> Self {
        Self { bytes, bit_pos: 0 }
    }

    /// Number of bits remaining (counting the byte-aligned tail bits
    /// that haven't been read yet).
    pub fn bits_remaining(&self) -> usize {
        self.bytes
            .len()
            .saturating_mul(8)
            .saturating_sub(self.bit_pos)
    }

    /// Confirm that at least `n` bits are available, returning an
    /// error otherwise.
    fn ensure_bits(&self, n: usize) -> FlatResult<()> {
        if n > self.bits_remaining() {
            return Err(UplcError::FlatDecode(format!(
                "unexpected end of input (wanted {n} bits, have {})",
                self.bits_remaining()
            )));
        }
        Ok(())
    }

    /// Read one bit as a `bool`.
    pub fn read_bit(&mut self) -> FlatResult<bool> {
        self.ensure_bits(1)?;
        let byte = self.bytes[self.bit_pos / 8];
        let mask = 1u8 << (7 - (self.bit_pos % 8));
        self.bit_pos += 1;
        Ok(byte & mask != 0)
    }

    /// Read `n` bits (with `n <= 8`) and return them as the low bits
    /// of a `u8`. Bits are packed MSB-first.
    pub fn read_bits8(&mut self, n: u8) -> FlatResult<u8> {
        if n == 0 {
            return Ok(0);
        }
        if n > 8 {
            return Err(UplcError::FlatDecode(format!(
                "read_bits8: n={n} exceeds 8"
            )));
        }
        self.ensure_bits(n as usize)?;
        let mut out = 0u8;
        for _ in 0..n {
            let byte = self.bytes[self.bit_pos / 8];
            let mask = 1u8 << (7 - (self.bit_pos % 8));
            self.bit_pos += 1;
            out = (out << 1) | u8::from(byte & mask != 0);
        }
        Ok(out)
    }

    /// Skip filler bits to the next byte boundary. The filler must
    /// have the form `1 0*` — a single set bit followed by zero or
    /// more zero bits up to the boundary. This is how the flat
    /// encoder marks the end of bit-packed regions before a
    /// byte-aligned payload (e.g. byte string chunks).
    pub fn read_filler(&mut self) -> FlatResult<()> {
        // Always at least one filler bit (the leading '1').
        let mut saw_one = false;
        loop {
            let in_byte = self.bit_pos % 8;
            if in_byte == 0 && saw_one {
                // We've already consumed the '1' and any trailing
                // '0's; we're now byte-aligned at the next boundary.
                return Ok(());
            }
            let bit = self.read_bit()?;
            match (saw_one, bit) {
                (false, true) => saw_one = true,
                (false, false) => {
                    return Err(UplcError::FlatDecode(
                        "filler must start with a 1 bit".into(),
                    ));
                }
                (true, false) => {
                    // Trailing zero — keep consuming until byte aligned.
                    if self.bit_pos.is_multiple_of(8) {
                        return Ok(());
                    }
                }
                (true, true) => {
                    return Err(UplcError::FlatDecode(
                        "filler may contain only one 1 bit".into(),
                    ));
                }
            }
        }
    }

    /// Read a flat-encoded `Natural` (unsigned arbitrary-precision
    /// integer) as a `u64`. The encoding is a sequence of 7-bit
    /// chunks (low bits first), each prefixed by a 1-bit continuation
    /// flag — `1` = more chunks follow, `0` = last chunk.
    ///
    /// Returns `FlatDecode("varint exceeds u64")` if the accumulated
    /// shift would exceed 64 bits — no silent wraparound.
    pub fn read_natural_u64(&mut self) -> FlatResult<u64> {
        let mut value: u64 = 0;
        let mut shift: u32 = 0;
        loop {
            let more = self.read_bit()?;
            let chunk = self.read_bits8(7)? as u64;
            if shift >= u64::BITS {
                return Err(UplcError::FlatDecode(
                    "Natural varint exceeds u64 range".into(),
                ));
            }
            value = value
                .checked_add(chunk << shift)
                .ok_or_else(|| UplcError::FlatDecode("Natural varint overflow".into()))?;
            if !more {
                return Ok(value);
            }
            shift += 7;
        }
    }

    /// Read a flat-encoded `Integer` (signed arbitrary-precision
    /// integer). The encoding is zig-zag from `Natural`:
    /// `n >= 0` → `2n`; `n < 0` → `2|n| - 1`.
    pub fn read_integer_i64(&mut self) -> FlatResult<i64> {
        let z = self.read_natural_u64()?;
        let n = (z >> 1) as i64;
        Ok(if z & 1 == 0 { n } else { -n - 1 })
    }

    /// Read a flat-encoded byte string. The encoding is: filler to
    /// byte boundary, then repeated chunks of `[len: u8, bytes: u8 *
    /// len]` until `len == 0`.
    pub fn read_bytestring(&mut self) -> FlatResult<Vec<u8>> {
        self.read_filler()?;
        let mut out = Vec::new();
        loop {
            // The 8-bit length lives at the byte boundary — read raw
            // bytes rather than going through the bit reader, since
            // we're byte-aligned.
            let pos = self.bit_pos / 8;
            if pos >= self.bytes.len() {
                return Err(UplcError::FlatDecode("byte-string: missing length".into()));
            }
            let len = self.bytes[pos] as usize;
            self.bit_pos += 8;
            if len == 0 {
                return Ok(out);
            }
            let start = self.bit_pos / 8;
            let end = start
                .checked_add(len)
                .ok_or_else(|| UplcError::FlatDecode("byte-string: length overflow".into()))?;
            if end > self.bytes.len() {
                return Err(UplcError::FlatDecode(format!(
                    "byte-string: chunk of {len} bytes past EOF"
                )));
            }
            out.extend_from_slice(&self.bytes[start..end]);
            self.bit_pos = end * 8;
        }
    }
}

/// Bit-level writer building up a `Vec<u8>` of flat-encoded bytes.
#[derive(Debug, Default)]
pub struct BitWriter {
    bytes: Vec<u8>,
    /// Total bits written.
    bit_pos: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append one bit.
    pub fn write_bit(&mut self, bit: bool) {
        if self.bit_pos.is_multiple_of(8) {
            self.bytes.push(0);
        }
        if bit {
            let byte_idx = self.bit_pos / 8;
            let in_byte = self.bit_pos % 8;
            self.bytes[byte_idx] |= 1u8 << (7 - in_byte);
        }
        self.bit_pos += 1;
    }

    /// Append the low `n` bits of `value` MSB-first. `n` must be
    /// `<= 8`.
    pub fn write_bits8(&mut self, value: u8, n: u8) -> FlatResult<()> {
        if n > 8 {
            return Err(UplcError::Encode(format!("write_bits8: n={n} exceeds 8")));
        }
        for i in (0..n).rev() {
            self.write_bit((value >> i) & 1 != 0);
        }
        Ok(())
    }

    /// Pad to the next byte boundary with the canonical `1 0*`
    /// filler.
    pub fn write_filler(&mut self) {
        self.write_bit(true);
        while !self.bit_pos.is_multiple_of(8) {
            self.write_bit(false);
        }
    }

    /// Append a `Natural` as 7-bit chunks with continuation flags.
    pub fn write_natural_u64(&mut self, mut value: u64) -> FlatResult<()> {
        loop {
            let chunk = (value & 0x7f) as u8;
            value >>= 7;
            let more = value != 0;
            self.write_bit(more);
            self.write_bits8(chunk, 7)?;
            if !more {
                return Ok(());
            }
        }
    }

    /// Append an `Integer` (zig-zag encoded).
    pub fn write_integer_i64(&mut self, value: i64) -> FlatResult<()> {
        let zigzag: u64 = if value >= 0 {
            (value as u64) << 1
        } else {
            ((!value as u64) << 1) | 1
        };
        self.write_natural_u64(zigzag)
    }

    /// Append a byte string with the filler prefix + chunked length
    /// encoding.
    pub fn write_bytestring(&mut self, bs: &[u8]) -> FlatResult<()> {
        self.write_filler();
        for chunk in bs.chunks(255) {
            // Already byte-aligned (filler did that, and each chunk
            // pushes whole bytes).
            self.bytes.push(chunk.len() as u8);
            self.bit_pos += 8;
            self.bytes.extend_from_slice(chunk);
            self.bit_pos += chunk.len() * 8;
        }
        // Terminator zero-length chunk.
        self.bytes.push(0);
        self.bit_pos += 8;
        Ok(())
    }

    /// Finalise the writer, padding to a byte boundary with the
    /// `1 0*` filler that the flat spec requires at the end of every
    /// encoded program.
    pub fn finish(mut self) -> Vec<u8> {
        self.write_filler();
        self.bytes
    }

    /// Length in bits of the encoded output so far.
    pub fn len_bits(&self) -> usize {
        self.bit_pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_natural(n: u64) {
        let mut w = BitWriter::new();
        w.write_natural_u64(n).unwrap();
        // For Natural-only tests we deliberately don't finalise (no
        // filler), so the reader sees only the natural bits.
        let bytes = w.bytes;
        let mut r = BitReader::new(&bytes);
        let got = r.read_natural_u64().unwrap();
        assert_eq!(got, n, "natural round-trip");
    }

    #[test]
    fn natural_small() {
        for n in [0u64, 1, 2, 7, 63, 64, 127, 128, 255, 256, 16383, 16384] {
            roundtrip_natural(n);
        }
    }

    #[test]
    fn natural_large() {
        for n in [u32::MAX as u64, u64::MAX / 2, u64::MAX] {
            roundtrip_natural(n);
        }
    }

    #[test]
    fn integer_roundtrip() {
        for n in [i64::MIN, -1_000_000, -1, 0, 1, 1_000_000, i64::MAX] {
            let mut w = BitWriter::new();
            w.write_integer_i64(n).unwrap();
            let bytes = w.bytes;
            let mut r = BitReader::new(&bytes);
            let got = r.read_integer_i64().unwrap();
            assert_eq!(got, n, "integer round-trip n={n}");
        }
    }

    #[test]
    fn bytestring_roundtrip() {
        for input in [
            Vec::new(),
            vec![0u8],
            vec![1, 2, 3, 4, 5],
            (0..=255u8).collect(),
            (0..1000u32).map(|i| (i & 0xff) as u8).collect(),
        ] {
            let mut w = BitWriter::new();
            w.write_bytestring(&input).unwrap();
            let bytes = w.bytes;
            let mut r = BitReader::new(&bytes);
            let got = r.read_bytestring().unwrap();
            assert_eq!(got, input, "bytestring round-trip len={}", input.len());
        }
    }

    #[test]
    fn read_past_end_errors() {
        let mut r = BitReader::new(&[]);
        assert!(r.read_bit().is_err());
        let mut r = BitReader::new(&[0xff]);
        for _ in 0..8 {
            r.read_bit().unwrap();
        }
        assert!(r.read_bit().is_err());
    }

    #[test]
    fn natural_overflow_errors() {
        // 11 chunks of 0x7f with continuation on the first 10 → the
        // accumulated shift reaches 70 at the start of iteration 11,
        // which exceeds u64::BITS=64. The reader must error rather
        // than silently wrapping (or panicking on shift overflow).
        let mut w = BitWriter::new();
        for i in 0..11 {
            w.write_bit(i < 10); // continuation on all but the last
            w.write_bits8(0x7f, 7).unwrap();
        }
        let bytes = w.bytes;
        let mut r = BitReader::new(&bytes);
        let err = r.read_natural_u64().unwrap_err();
        assert!(matches!(err, UplcError::FlatDecode(_)), "got {err:?}");
    }

    #[test]
    fn bytestring_truncated_chunk_errors() {
        // Header byte says "5 bytes follow" but only 3 remain.
        let bytes = vec![0b1000_0000, 0x05, 0x01, 0x02, 0x03];
        let mut r = BitReader::new(&bytes);
        let err = r.read_bytestring().unwrap_err();
        assert!(matches!(err, UplcError::FlatDecode(_)));
    }

    #[test]
    fn filler_accepts_minimal() {
        let mut w = BitWriter::new();
        w.write_filler();
        let bytes = w.bytes;
        let mut r = BitReader::new(&bytes);
        r.read_filler().unwrap();
        assert_eq!(r.bit_pos % 8, 0);
    }

    #[test]
    fn filler_rejects_no_leading_one() {
        // A byte of all zeros — no leading 1 bit.
        let bytes = vec![0u8];
        let mut r = BitReader::new(&bytes);
        assert!(r.read_filler().is_err());
    }
}
