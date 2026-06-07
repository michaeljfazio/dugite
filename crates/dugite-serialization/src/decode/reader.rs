//! CBOR reader wrapping `minicbor::Decoder` with offset tracking.
//!
//! [`Reader`] is the primary interface for all in-house CBOR decoders. It adds:
//!
//! - **Offset tracking** via [`Reader::position`] and [`Reader::slice_from`], which
//!   allow [`crate::decode::raw::KeepRaw`] to take zero-copy slices of the original buffer.
//! - **CBOR tag helpers** for the tag types Cardano uses: tag 2/3 bignums, tag 24
//!   embedded CBOR, tag 30 rationals, and tag 258 sets.
//! - **`read_set`** that transparently handles both tagged (Conway+) and untagged
//!   (pre-Conway) set encoding.
//! - **`read_indef_bytes`** that concatenates indefinite-length byte string chunks
//!   in wire order — critical for Plutus data where the exact chunk boundaries
//!   affect the `script_data_hash`.
//!
//! The underlying byte-exact CBOR skip primitive
//! (`crate::haskell_snapshot::cbor_utils::skip_cbor_value`) is reused here so that
//! the `skip` method is validated against the Haskell node's encoder.

use crate::error::SerializationError;
use crate::haskell_snapshot::cbor_utils::skip_cbor_value;
use dugite_primitives::transaction::Rational;
use minicbor::data::Type;
use minicbor::Decoder;
use num_bigint::BigInt;

/// CBOR tag for a finite rational number as defined in RFC 8949 §3.4.
/// Encoding: `tag(30) [numerator, denominator]`.
const TAG_RATIONAL: u64 = 30;

/// CBOR tag 2: positive bignum (big-endian byte string).
const TAG_BIGNUM_POS: u64 = 2;

/// CBOR tag 3: negative bignum (big-endian byte string; value = -1 - n).
const TAG_BIGNUM_NEG: u64 = 3;

/// CBOR tag 24: embedded CBOR (byte string containing a CBOR value).
pub const TAG_EMBEDDED_CBOR: u64 = 24;

/// CBOR tag 258: mathematical finite set.
///
/// Conway-era CDDL uses `set<a> = #6.258([* a])`. Pre-Conway eras use plain
/// arrays. The [`Reader::read_set`] method handles both transparently so that
/// callers do not need era-specific dispatch.
const TAG_SET: u64 = 258;

/// Tag 258 encoded as two bytes: `0xd9 0x01 0x02` (major 6, info 25, value 258).
/// This is the only legal CBOR encoding for tag 258 (canonical form).
const TAG_258_HEADER: [u8; 3] = [0xd9, 0x01, 0x02];

/// A CBOR reader wrapping [`minicbor::Decoder`] with offset-tracking capability.
///
/// All public methods return [`crate::error::SerializationError`] on failure.
/// Methods that read structured types (arrays, maps, sets) advance the internal
/// position past the entire structure including all nested values.
///
/// # Offset tracking
///
/// [`Reader::position`] returns the number of bytes consumed from the start of
/// `origin`. [`Reader::slice_from`] takes a snapshot position and returns the
/// slice `origin[start..current_position]`, enabling zero-copy raw-byte
/// extraction for hashing (tx body, witness set, etc.).
pub struct Reader<'b> {
    /// Underlying minicbor decoder.
    inner: Decoder<'b>,
    /// The original input buffer; kept to allow zero-copy byte slicing.
    origin: &'b [u8],
}

impl<'b> Reader<'b> {
    /// Create a new `Reader` backed by the given byte slice.
    ///
    /// The slice is passed to `minicbor::Decoder` and also retained as `origin`
    /// for zero-copy slicing via [`Reader::slice_from`].
    pub fn new(buf: &'b [u8]) -> Self {
        Self {
            inner: Decoder::new(buf),
            origin: buf,
        }
    }

    /// Return the current byte offset from the start of the input.
    #[inline]
    pub fn position(&self) -> usize {
        self.inner.position()
    }

    /// Return the original input slice from `start` to the current position.
    ///
    /// Used by [`crate::decode::raw::KeepRaw::parse_with`] to capture the raw bytes
    /// of a CBOR value without copying them during decode.
    ///
    /// # Panics (debug only)
    /// Panics in debug builds if `start > self.position()`.
    #[inline]
    pub fn slice_from(&self, start: usize) -> &'b [u8] {
        debug_assert!(
            start <= self.inner.position(),
            "slice_from: start={start} > position={}",
            self.inner.position()
        );
        &self.origin[start..self.inner.position()]
    }

    /// Peek at the CBOR major type at the current position without advancing.
    ///
    /// Returns an error if the buffer is exhausted.
    #[inline]
    pub fn peek_major(&self) -> Result<Type, SerializationError> {
        self.inner
            .datatype()
            .map_err(|e| SerializationError::CborDecode(format!("peek_major: {e}")))
    }

    /// Consume exactly one CBOR break byte (`0xff`) at the current position.
    ///
    /// Errors if the current value is not a break. This mirrors the upstream
    /// cardano-ledger `decodeBreakOr`/`unless isBreak $ cborError ... "Excess
    /// terms in array"` check that closes an indefinite-length list once its
    /// fixed body has been decoded (`decodeListLikeT`). Used to consume the
    /// trailing break of an indefinite-length structural array whose entries are
    /// read by position rather than via [`read_array`].
    pub fn expect_break(&mut self) -> Result<(), SerializationError> {
        match self.peek_major()? {
            Type::Break => {
                let pos = self.inner.position();
                self.inner.set_position(pos + 1);
                Ok(())
            }
            other => Err(SerializationError::CborDecode(format!(
                "expected CBOR break (0xff) to close indefinite-length array, found {other:?} \
                 (upstream `decodeListLikeT`: \"Excess terms in array\")"
            ))),
        }
    }

    /// Skip over exactly one CBOR value at the current position.
    ///
    /// Delegates to `crate::haskell_snapshot::cbor_utils::skip_cbor_value` which has
    /// been validated against the Haskell node's CBOR encoder and handles the full
    /// CBOR grammar including indefinite-length types, tags, and deeply nested structures.
    pub fn skip(&mut self) -> Result<(), SerializationError> {
        let pos = self.inner.position();
        let remaining = &self.origin[pos..];
        let consumed = skip_cbor_value(remaining)?;
        self.inner.set_position(pos + consumed);
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Array / map header readers
    // -------------------------------------------------------------------------

    /// Read an array header and return the length, or `None` for indefinite.
    ///
    /// Advances the position past the header byte(s) only; the caller must then
    /// read `n` items (or items until break for indefinite).
    ///
    /// This is the low-level primitive. Most callers should use [`read_array`].
    pub fn read_array_header(&mut self) -> Result<Option<u64>, SerializationError> {
        self.inner
            .array()
            .map_err(|e| SerializationError::CborDecode(format!("array header: {e}")))
    }

    /// Read a map header and return the entry count, or `None` for indefinite.
    ///
    /// Advances the position past the header byte(s) only.
    pub fn read_map_header(&mut self) -> Result<Option<u64>, SerializationError> {
        self.inner
            .map()
            .map_err(|e| SerializationError::CborDecode(format!("map header: {e}")))
    }

    /// Probe the array length at the current position **without** advancing.
    ///
    /// Returns `Some(n)` for a definite-length array of `n` items, or `None`
    /// for an indefinite-length array. Returns an error if the current value is
    /// not an array.
    pub fn probe_array_len(&mut self) -> Result<Option<u64>, SerializationError> {
        let saved = self.inner.position();
        let result = self.read_array_header()?;
        // Restore position — the caller is only probing.
        self.inner.set_position(saved);
        Ok(result)
    }

    /// Probe the map length at the current position **without** advancing.
    ///
    /// Returns `Some(n)` for a definite-length map, or `None` for indefinite.
    /// Returns an error if the current value is not a map.
    pub fn probe_map_len(&mut self) -> Result<Option<u64>, SerializationError> {
        let saved = self.inner.position();
        let result = self.read_map_header()?;
        self.inner.set_position(saved);
        Ok(result)
    }

    // -------------------------------------------------------------------------
    // Structural readers
    // -------------------------------------------------------------------------

    /// Read a CBOR set, optionally prefixed with tag 258.
    ///
    /// Conway-era CDDL defines sets as `#6.258([* a])`. Pre-Conway eras use
    /// plain arrays. This method handles both transparently:
    ///
    /// - If the next byte is the tag-258 header (`0xd9 0x01 0x02`), it is consumed.
    /// - Then a definite- or indefinite-length array is read and each item decoded
    ///   with `item`.
    ///
    /// Both tagged and untagged inputs decode correctly; callers do not need to
    /// know which era they are in.
    pub fn read_set<T, F>(&mut self, item: F) -> Result<Vec<T>, SerializationError>
    where
        F: FnMut(&mut Reader<'b>) -> Result<T, SerializationError>,
    {
        // Peek at the next 3 bytes to detect tag 258.
        let pos = self.inner.position();
        let remaining = &self.origin[pos..];
        if remaining.starts_with(&TAG_258_HEADER) {
            // Consume the tag header (3 bytes).
            self.inner.set_position(pos + 3);
        }
        // Now read the array body.
        self.read_array(item)
    }

    /// Read a CBOR set, optionally prefixed with tag 258, **rejecting duplicate
    /// elements** (Conway PV9+ semantics).
    ///
    /// This is the strict counterpart to [`read_set`]. Use it only at Conway-era
    /// (and later) decoder sites, which are statically protocol-version 9 or
    /// above. Pre-Conway eras must keep using the lenient [`read_set`].
    ///
    /// # Upstream parity
    ///
    /// `cardano-ledger-binary`'s `decodeSet` is protocol-version gated: at PV9+
    /// it routes through `decodeSetEnforceNoDuplicates` →
    /// `decodeListLikeEnforceNoDuplicates`, which decodes every physical array
    /// element, inserts them into a `Set` (Ord-dedup), then does
    /// `when (len /= count) $ fail` — where `count` is the number of physical
    /// elements decoded and `len` is the size of the deduplicated `Set`. Any
    /// duplicate makes `len < count` and hard-fails the whole decode. Pre-PV9 is
    /// lenient (`Set.fromList` silently drops duplicates). Ordering is never
    /// enforced (the tag-258 prefix is optional and elements are not required to
    /// be sorted), so this method preserves wire order and only rejects dups.
    ///
    /// # Duplicate detection
    ///
    /// Duplicates are detected by the **raw CBOR byte span** of each decoded
    /// element (captured via `position`/`slice_from`), tracked in a
    /// `HashSet<Vec<u8>>`. For canonical on-chain CBOR this coincides exactly
    /// with Haskell's value-`Ord` dedup, because each value has a single
    /// canonical encoding. Residual edge: two *non-canonical* encodings of the
    /// same value with *different* bytes would pass raw-byte dedup here but be
    /// rejected by Haskell's value-`Ord` dedup — a theoretical adversarial case
    /// that does not arise for canonically-encoded chain data.
    pub fn read_set_strict<T, F>(&mut self, mut item: F) -> Result<Vec<T>, SerializationError>
    where
        F: FnMut(&mut Reader<'b>) -> Result<T, SerializationError>,
    {
        // Strip the optional tag-258 header exactly as `read_set` does.
        let pos = self.inner.position();
        let remaining = &self.origin[pos..];
        if remaining.starts_with(&TAG_258_HEADER) {
            self.inner.set_position(pos + 3);
        }

        // Decode the array body, counting physical elements and tracking the
        // raw byte span of each so that any duplicate is a hard error.
        let len = self.read_array_header()?;
        let mut seen: std::collections::HashSet<Vec<u8>> = std::collections::HashSet::new();
        match len {
            Some(n) => {
                let mut out = Vec::with_capacity(self.safe_alloc_capacity(n));
                for _ in 0..n {
                    let start = self.inner.position();
                    let value = item(self)?;
                    let raw = self.slice_from(start).to_vec();
                    if !seen.insert(raw) {
                        return Err(SerializationError::CborDecode(
                            "set: duplicate element".to_string(),
                        ));
                    }
                    out.push(value);
                }
                Ok(out)
            }
            None => {
                // Indefinite-length: read until the break byte.
                let mut out = Vec::new();
                loop {
                    let ty = self.peek_major()?;
                    if ty == Type::Break {
                        let pos = self.inner.position();
                        self.inner.set_position(pos + 1);
                        break;
                    }
                    let start = self.inner.position();
                    let value = item(self)?;
                    let raw = self.slice_from(start).to_vec();
                    if !seen.insert(raw) {
                        return Err(SerializationError::CborDecode(
                            "set: duplicate element".to_string(),
                        ));
                    }
                    out.push(value);
                }
                Ok(out)
            }
        }
    }

    /// Read a definite- or indefinite-length CBOR array, decoding each item.
    ///
    /// For indefinite-length arrays the reader stops at the CBOR break byte (0xff).
    pub fn read_array<T, F>(&mut self, mut item: F) -> Result<Vec<T>, SerializationError>
    where
        F: FnMut(&mut Reader<'b>) -> Result<T, SerializationError>,
    {
        let len = self.read_array_header()?;
        match len {
            Some(n) => {
                let mut out = Vec::with_capacity(self.safe_alloc_capacity(n));
                for _ in 0..n {
                    out.push(item(self)?);
                }
                Ok(out)
            }
            None => {
                // Indefinite-length: read until break byte.
                let mut out = Vec::new();
                loop {
                    let ty = self.peek_major()?;
                    if ty == Type::Break {
                        // Consume the break byte.
                        let pos = self.inner.position();
                        self.inner.set_position(pos + 1);
                        break;
                    }
                    out.push(item(self)?);
                }
                Ok(out)
            }
        }
    }

    /// Cap the initial allocation for a peer-controlled CBOR length header.
    ///
    /// Every CBOR value occupies at least one byte, so a declared array/map
    /// length larger than the number of bytes still in the input cannot
    /// honestly represent that many items. Clamping `Vec::with_capacity` to
    /// `min(declared, remaining_bytes)` prevents an attacker from forcing a
    /// multi-exabyte allocation via a single forged length header
    /// (audit #544 / #554).
    ///
    /// This is a *hint only*. The decode loop still runs `n` iterations and
    /// errors out the moment a per-item decode hits end-of-input, so an
    /// over-declared length is rejected within microseconds.
    pub(crate) fn safe_alloc_capacity(&self, declared: u64) -> usize {
        let pos = self.inner.position();
        let remaining_bytes = self.origin.len().saturating_sub(pos);
        usize::try_from(declared)
            .unwrap_or(usize::MAX)
            .min(remaining_bytes)
    }

    /// Iterate the items of an array (definite or indefinite) without buffering
    /// the items into a `Vec`. Invokes `item(self)` for each element; for an
    /// indefinite-length array the loop stops at the CBOR break byte. Use this
    /// when the per-item handling needs the [`Reader`] state directly (e.g. to
    /// build `KeepRaw` wrappers) and a regular `Vec` of values is undesirable.
    pub fn for_each_array_item<F>(&mut self, mut item: F) -> Result<(), SerializationError>
    where
        F: FnMut(&mut Reader<'b>) -> Result<(), SerializationError>,
    {
        let len = self.read_array_header()?;
        match len {
            Some(n) => {
                for _ in 0..n {
                    item(self)?;
                }
            }
            None => loop {
                let ty = self.peek_major()?;
                if ty == Type::Break {
                    let pos = self.inner.position();
                    self.inner.set_position(pos + 1);
                    break;
                }
                item(self)?;
            },
        }
        Ok(())
    }

    /// Iterate the entries of a map (definite or indefinite). The closure must
    /// consume exactly one CBOR key + one CBOR value per call. For
    /// indefinite-length maps the loop stops at the CBOR break byte.
    pub fn for_each_map_entry<F>(&mut self, mut entry: F) -> Result<(), SerializationError>
    where
        F: FnMut(&mut Reader<'b>) -> Result<(), SerializationError>,
    {
        let len = self.read_map_header()?;
        match len {
            Some(n) => {
                for _ in 0..n {
                    entry(self)?;
                }
            }
            None => loop {
                let ty = self.peek_major()?;
                if ty == Type::Break {
                    let pos = self.inner.position();
                    self.inner.set_position(pos + 1);
                    break;
                }
                entry(self)?;
            },
        }
        Ok(())
    }

    /// Read a definite- or indefinite-length CBOR map, decoding each key-value pair.
    ///
    /// Entries are returned in the order they appear in the input; no deduplication
    /// or ordering is applied. Duplicate keys keep the last value (Haskell behaviour).
    pub fn read_map<K, V, FK, FV>(
        &mut self,
        mut k: FK,
        mut v: FV,
    ) -> Result<Vec<(K, V)>, SerializationError>
    where
        FK: FnMut(&mut Reader<'b>) -> Result<K, SerializationError>,
        FV: FnMut(&mut Reader<'b>) -> Result<V, SerializationError>,
    {
        let len = self.read_map_header()?;
        match len {
            Some(n) => {
                let mut out = Vec::with_capacity(self.safe_alloc_capacity(n));
                for _ in 0..n {
                    let key = k(self)?;
                    let val = v(self)?;
                    out.push((key, val));
                }
                Ok(out)
            }
            None => {
                let mut out = Vec::new();
                loop {
                    let ty = self.peek_major()?;
                    if ty == Type::Break {
                        let pos = self.inner.position();
                        self.inner.set_position(pos + 1);
                        break;
                    }
                    let key = k(self)?;
                    let val = v(self)?;
                    out.push((key, val));
                }
                Ok(out)
            }
        }
    }

    // -------------------------------------------------------------------------
    // Primitive readers
    // -------------------------------------------------------------------------

    /// Read a definite-length CBOR byte string (major type 2), returning a
    /// zero-copy slice of the original buffer.
    ///
    /// Indefinite-length byte strings (0x5f) are rejected here; use
    /// [`read_indef_bytes`] or [`read_bytes_owned`] for those.
    pub fn read_bytes(&mut self) -> Result<&'b [u8], SerializationError> {
        self.inner
            .bytes()
            .map_err(|e| SerializationError::CborDecode(format!("bytes: {e}")))
    }

    /// Read a CBOR byte string (definite or indefinite) and return an owned `Vec<u8>`.
    ///
    /// For definite-length strings this copies the bytes once.
    /// For indefinite-length strings the chunks are concatenated.
    pub fn read_bytes_owned(&mut self) -> Result<Vec<u8>, SerializationError> {
        match self.peek_major()? {
            Type::BytesIndef => self.read_indef_bytes(),
            _ => Ok(self.read_bytes()?.to_vec()),
        }
    }

    /// Read a definite-length CBOR text string (major type 3), returning a
    /// zero-copy `&str` slice of the original buffer.
    ///
    /// Used for transaction metadata text values (`TransactionMetadatum::Text`).
    pub fn read_str(&mut self) -> Result<&'b str, SerializationError> {
        self.inner
            .str()
            .map_err(|e| SerializationError::CborDecode(format!("str: {e}")))
    }

    /// Read an indefinite-length CBOR byte string (`0x5f ... 0xff`), concatenating
    /// all chunks in wire order.
    ///
    /// Wire-order concatenation is load-bearing: Plutus `PlutusData` byte strings
    /// may be chunked across multiple CBOR byte string segments. Re-chunking or
    /// reordering would change the `script_data_hash`. This method preserves the
    /// concatenated content exactly as produced by the Haskell encoder.
    ///
    /// If the current value is a definite-length byte string, it is decoded as-is
    /// without copying.
    pub fn read_indef_bytes(&mut self) -> Result<Vec<u8>, SerializationError> {
        let ty = self.peek_major()?;
        if ty != Type::BytesIndef {
            // Definite-length fallback — still copy to satisfy the owned return.
            return Ok(self.read_bytes()?.to_vec());
        }
        // Consume the 0x5f header byte.
        let pos = self.inner.position();
        self.inner.set_position(pos + 1);

        let mut out = Vec::new();
        loop {
            let ty = self.peek_major()?;
            match ty {
                Type::Break => {
                    // Consume the break byte (0xff).
                    let pos = self.inner.position();
                    self.inner.set_position(pos + 1);
                    break;
                }
                Type::Bytes => {
                    let chunk = self.read_bytes()?;
                    out.extend_from_slice(chunk);
                }
                other => {
                    return Err(SerializationError::CborDecode(format!(
                        "read_indef_bytes: expected Bytes or Break, got {other}"
                    )));
                }
            }
        }
        Ok(out)
    }

    /// Maximum length of a single `PlutusData` `ByteString` *leaf chunk*, in bytes.
    ///
    /// Mirrors the Haskell `plutus` `PlutusCore.Data.decodeData` "Note [The
    /// 64-byte limit]": `decodeBoundedBytes` / `decodeBoundedBytesIndefLen`
    /// reject any single CBOR byte-string chunk longer than 64 bytes. For the
    /// indefinite-length chunked form, the bound applies *per chunk* — the
    /// concatenated total may exceed 64 bytes across multiple `<= 64`-byte
    /// chunks. A zero-length chunk is permitted.
    pub(crate) const PLUTUS_DATA_BYTES_LEAF_MAX: usize = 64;

    /// Read a `PlutusData` `ByteString` *leaf* (definite or indefinite-length),
    /// enforcing the plutus 64-byte-per-chunk bound and returning an owned
    /// `Vec<u8>`.
    ///
    /// This is the bounded counterpart of [`Reader::read_bytes_owned`] /
    /// [`Reader::read_indef_bytes`], to be used **only** at `PlutusData` leaf
    /// sites (the `Bytes`/`BytesIndef` arms and the tag-2/tag-3 bignum
    /// mantissa). It matches Haskell `plutus`
    /// `PlutusCore.Data.decodeBoundedBytes` / `decodeBoundedBytesIndefLen`:
    ///
    /// - A single **definite**-length byte string longer than 64 bytes is
    ///   rejected.
    /// - For the **indefinite**-length chunked form, *each individual chunk*
    ///   must be `<= 64` bytes (any single chunk `> 64` is rejected); the
    ///   concatenated total is **not** bounded. A zero-length chunk is allowed.
    ///
    /// The generic readers ([`Reader::read_bytes_owned`],
    /// [`Reader::read_indef_bytes`]) deliberately stay unbounded — they serve
    /// non-`PlutusData` callers (Ed25519 vkeys, KES/VRF, native + Plutus script
    /// blobs which routinely exceed 64 bytes, addresses, asset names, metadata)
    /// that are *not* subject to the plutus 64-byte rule.
    pub(crate) fn read_bounded_plutus_bytes(&mut self) -> Result<Vec<u8>, SerializationError> {
        match self.peek_major()? {
            Type::BytesIndef => {
                // Consume the 0x5f header byte.
                let pos = self.inner.position();
                self.inner.set_position(pos + 1);

                let mut out = Vec::new();
                loop {
                    let ty = self.peek_major()?;
                    match ty {
                        Type::Break => {
                            // Consume the break byte (0xff).
                            let pos = self.inner.position();
                            self.inner.set_position(pos + 1);
                            break;
                        }
                        Type::Bytes => {
                            let chunk = self.read_bytes()?;
                            if chunk.len() > Self::PLUTUS_DATA_BYTES_LEAF_MAX {
                                return Err(SerializationError::CborDecode(format!(
                                    "PlutusData ByteString leaf exceeds 64 bytes \
                                     (indefinite-length chunk of {} bytes)",
                                    chunk.len()
                                )));
                            }
                            out.extend_from_slice(chunk);
                        }
                        other => {
                            return Err(SerializationError::CborDecode(format!(
                                "read_bounded_plutus_bytes: expected Bytes or Break, got {other}"
                            )));
                        }
                    }
                }
                Ok(out)
            }
            _ => {
                let bytes = self.read_bytes()?;
                if bytes.len() > Self::PLUTUS_DATA_BYTES_LEAF_MAX {
                    return Err(SerializationError::CborDecode(format!(
                        "PlutusData ByteString leaf exceeds 64 bytes \
                         (definite-length string of {} bytes)",
                        bytes.len()
                    )));
                }
                Ok(bytes.to_vec())
            }
        }
    }

    /// Read a CBOR unsigned integer (major type 0).
    pub fn read_uint(&mut self) -> Result<u64, SerializationError> {
        self.inner
            .u64()
            .map_err(|e| SerializationError::CborDecode(format!("uint: {e}")))
    }

    /// Read a CBOR integer (major type 0 or 1), returning an `i128` to handle
    /// the full signed range without overflow.
    ///
    /// CBOR major-type-1 encodes negative integers as `-1 - n` where `n` is the
    /// encoded magnitude. The `minicbor::Int` type covers the exact range
    /// `[-2^64, 2^64 - 1]` which exceeds `i64` and `i128::MIN`.
    pub fn read_int(&mut self) -> Result<i128, SerializationError> {
        let pos = self.inner.position();
        let ty = self.peek_major()?;
        match ty {
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                let v = self
                    .inner
                    .u64()
                    .map_err(|e| SerializationError::CborDecode(format!("int/uint: {e}")))?;
                Ok(v as i128)
            }
            Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::Int => {
                // minicbor::Int covers the range; conversion to i128 is infallible.
                let v: minicbor::data::Int = self
                    .inner
                    .int()
                    .map_err(|e| SerializationError::CborDecode(format!("int: {e}")))?;
                Ok(i128::from(v))
            }
            other => Err(SerializationError::CborDecode(format!(
                "read_int: expected integer, got {other} at position {pos}"
            ))),
        }
    }

    /// Read a CBOR integer or bignum, returning an arbitrary-precision `BigInt`.
    ///
    /// Handles four CBOR shapes:
    /// - Major type 0 (unsigned): non-negative integer.
    /// - Major type 1 (negative): negative integer.
    /// - Tag 2 + bytestring: positive bignum (`value = bytes interpreted as big-endian u256+`).
    /// - Tag 3 + bytestring: negative bignum (`value = -1 - bytes_as_bigint`).
    ///
    /// This covers all integer encodings in Cardano Plutus data and protocol parameters.
    pub fn read_bigint(&mut self) -> Result<BigInt, SerializationError> {
        let pos = self.inner.position();
        let ty = self.peek_major()?;
        match ty {
            Type::Tag => {
                let tag_val = self
                    .inner
                    .tag()
                    .map_err(|e| SerializationError::CborDecode(format!("bigint tag: {e}")))?;
                match tag_val.as_u64() {
                    TAG_BIGNUM_POS => {
                        // CBOR §3.4.3 + Cardano `bounded_bytes`: bignum
                        // mantissa may be indefinite-length. See #673.
                        let bytes = self.read_bytes_owned()?;
                        Ok(BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes))
                    }
                    TAG_BIGNUM_NEG => {
                        let bytes = self.read_bytes_owned()?;
                        // value = -1 - n  where n = BigInt::from_bytes_be(+, bytes)
                        let magnitude = BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes);
                        Ok(-BigInt::from(1) - magnitude)
                    }
                    other => Err(SerializationError::CborDecode(format!(
                        "read_bigint: unexpected tag {other} at position {pos}"
                    ))),
                }
            }
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                let v = self
                    .inner
                    .u64()
                    .map_err(|e| SerializationError::CborDecode(format!("bigint/uint: {e}")))?;
                Ok(BigInt::from(v))
            }
            Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::Int => {
                let v: minicbor::data::Int = self
                    .inner
                    .int()
                    .map_err(|e| SerializationError::CborDecode(format!("bigint/int: {e}")))?;
                Ok(BigInt::from(i128::from(v)))
            }
            other => Err(SerializationError::CborDecode(format!(
                "read_bigint: expected integer or bignum tag, got {other} at position {pos}"
            ))),
        }
    }

    /// Read a `PlutusData` integer (small int **or** tag-2/tag-3 bignum),
    /// enforcing the plutus 64-byte-per-chunk bound on the bignum *mantissa*.
    ///
    /// This is the `PlutusData`-only counterpart of [`Reader::read_bigint`].
    /// The bignum mantissa is a `PlutusData` `ByteString` leaf, so it must obey
    /// Note [The 64-byte limit] just like the `Bytes`/`BytesIndef` leaf arms:
    /// a definite mantissa `> 64` bytes is rejected, and each indefinite
    /// mantissa chunk must be `<= 64` bytes (total unbounded).
    ///
    /// `read_bigint` itself is left unbounded so its (current and future)
    /// non-`PlutusData` callers are not over-restricted; only the `PlutusData`
    /// decode arms route bignums through this bounded helper.
    pub(crate) fn read_bounded_plutus_bigint(&mut self) -> Result<BigInt, SerializationError> {
        let pos = self.inner.position();
        let ty = self.peek_major()?;
        match ty {
            Type::Tag => {
                let tag_val = self
                    .inner
                    .tag()
                    .map_err(|e| SerializationError::CborDecode(format!("bigint tag: {e}")))?;
                match tag_val.as_u64() {
                    TAG_BIGNUM_POS => {
                        let bytes = self.read_bounded_plutus_bytes()?;
                        Ok(BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes))
                    }
                    TAG_BIGNUM_NEG => {
                        let bytes = self.read_bounded_plutus_bytes()?;
                        // value = -1 - n  where n = BigInt::from_bytes_be(+, bytes)
                        let magnitude = BigInt::from_bytes_be(num_bigint::Sign::Plus, &bytes);
                        Ok(-BigInt::from(1) - magnitude)
                    }
                    other => Err(SerializationError::CborDecode(format!(
                        "read_bounded_plutus_bigint: unexpected tag {other} at position {pos}"
                    ))),
                }
            }
            Type::U8 | Type::U16 | Type::U32 | Type::U64 => {
                let v = self
                    .inner
                    .u64()
                    .map_err(|e| SerializationError::CborDecode(format!("bigint/uint: {e}")))?;
                Ok(BigInt::from(v))
            }
            Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::Int => {
                let v: minicbor::data::Int = self
                    .inner
                    .int()
                    .map_err(|e| SerializationError::CborDecode(format!("bigint/int: {e}")))?;
                Ok(BigInt::from(i128::from(v)))
            }
            other => Err(SerializationError::CborDecode(format!(
                "read_bounded_plutus_bigint: expected integer or bignum tag, got {other} at position {pos}"
            ))),
        }
    }

    /// Read a CBOR rational number encoded as tag 30 + `[numerator, denominator]`.
    ///
    /// Both the numerator and denominator are decoded with [`read_uint`] (they are
    /// always non-negative in Cardano protocol parameters). Returns a
    /// [`dugite_primitives::transaction::Rational`].
    ///
    /// # Wire format
    /// Tag 30 is defined in IANA CBOR Tag Registry for "Rational Numbers". The
    /// Cardano ledger uses it for protocol parameters like `a0`, `rho`, and `tau`.
    pub fn read_rational(&mut self) -> Result<Rational, SerializationError> {
        let pos = self.inner.position();
        // Consume tag 30.
        let tag = self
            .inner
            .tag()
            .map_err(|e| SerializationError::CborDecode(format!("rational tag: {e}")))?;
        if tag.as_u64() != TAG_RATIONAL {
            return Err(SerializationError::CborDecode(format!(
                "read_rational: expected tag 30, got tag {} at position {pos}",
                tag.as_u64()
            )));
        }
        // Consume the 2-element array header.
        let arr_len = self.read_array_header()?;
        if arr_len != Some(2) {
            return Err(SerializationError::CborDecode(format!(
                "read_rational: expected array(2), got {arr_len:?} at position {pos}"
            )));
        }
        let numerator = self.read_uint()?;
        let denominator = self.read_uint()?;
        if denominator == 0 {
            return Err(SerializationError::CborDecode(
                "read_rational: denominator is zero".into(),
            ));
        }
        Ok(Rational {
            numerator,
            denominator,
        })
    }

    /// Read a CBOR boolean value (`true` / `false`).
    pub fn read_bool(&mut self) -> Result<bool, SerializationError> {
        self.inner
            .bool()
            .map_err(|e| SerializationError::CborDecode(format!("bool: {e}")))
    }

    /// Read a CBOR null value (`0xf6`).
    ///
    /// Returns an error if the current value is not null.
    pub fn read_null(&mut self) -> Result<(), SerializationError> {
        self.inner
            .null()
            .map_err(|e| SerializationError::CborDecode(format!("null: {e}")))
    }

    /// Read a CBOR tag value and advance past it.
    ///
    /// Returns the numeric tag value. Unlike [`expect_tag`], this does not
    /// check the value — callers can switch on it themselves.
    pub fn read_tag(&mut self) -> Result<u64, SerializationError> {
        self.inner
            .tag()
            .map(|t| t.as_u64())
            .map_err(|e| SerializationError::CborDecode(format!("read_tag: {e}")))
    }

    /// Peek at the CBOR tag value at the current position **without** advancing.
    ///
    /// Returns an error if the current value is not a tag.
    pub fn probe_tag(&mut self) -> Result<u64, SerializationError> {
        let saved = self.inner.position();
        let tag_val = self.read_tag()?;
        self.inner.set_position(saved);
        Ok(tag_val)
    }

    /// Expect a specific CBOR tag at the current position and consume it.
    ///
    /// Advances past the tag header. Returns an error if the tag value does not
    /// match or if the current value is not a tag.
    pub fn expect_tag(&mut self, expected: u64) -> Result<(), SerializationError> {
        let pos = self.inner.position();
        let tag = self
            .inner
            .tag()
            .map_err(|e| SerializationError::CborDecode(format!("expect_tag({expected}): {e}")))?;
        if tag.as_u64() != expected {
            return Err(SerializationError::CborDecode(format!(
                "expect_tag: expected tag {expected}, got tag {} at position {pos}",
                tag.as_u64()
            )));
        }
        Ok(())
    }

    /// Read a tag 24 (embedded CBOR) byte string, returning the inner bytes.
    ///
    /// Tag 24 wraps a byte string whose content is itself valid CBOR. Used in
    /// Cardano for `InlineDatum` and some governance fields.
    pub fn read_embedded_cbor_bytes(&mut self) -> Result<&'b [u8], SerializationError> {
        self.expect_tag(TAG_EMBEDDED_CBOR)?;
        self.read_bytes()
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Helpers for building test CBOR
    // -----------------------------------------------------------------------

    /// Encode a definite-length uint (single byte if ≤ 23).
    fn cbor_uint(n: u64) -> Vec<u8> {
        if n <= 23 {
            vec![n as u8]
        } else if n <= 0xff {
            vec![0x18, n as u8]
        } else if n <= 0xffff {
            let b = (n as u16).to_be_bytes();
            vec![0x19, b[0], b[1]]
        } else if n <= 0xffff_ffff {
            let b = (n as u32).to_be_bytes();
            vec![0x1a, b[0], b[1], b[2], b[3]]
        } else {
            let b = n.to_be_bytes();
            vec![0x1b, b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]
        }
    }

    /// Encode a negative integer in CBOR major type 1.
    fn cbor_neg(n: u64) -> Vec<u8> {
        // CBOR encodes -1 - n in major type 1.
        let mut v = cbor_uint(n);
        v[0] |= 0x20;
        v
    }

    /// Encode a definite-length byte string.
    fn cbor_bytes(b: &[u8]) -> Vec<u8> {
        let mut hdr = cbor_uint(b.len() as u64);
        hdr[0] |= 0x40;
        hdr.extend_from_slice(b);
        hdr
    }

    /// Encode a definite-length array header only.
    fn cbor_array_hdr(n: usize) -> Vec<u8> {
        let mut v = cbor_uint(n as u64);
        v[0] |= 0x80;
        v
    }

    /// Encode a definite-length map header only.
    fn cbor_map_hdr(n: usize) -> Vec<u8> {
        let mut v = cbor_uint(n as u64);
        v[0] |= 0xa0;
        v
    }

    /// CBOR null.
    const CBOR_NULL: u8 = 0xf6;

    /// CBOR true.
    const CBOR_TRUE: u8 = 0xf5;

    /// CBOR false.
    const CBOR_FALSE: u8 = 0xf4;

    /// CBOR break byte.
    const CBOR_BREAK: u8 = 0xff;

    /// Encode tag(n) using the minimal-length encoding.
    fn cbor_tag(n: u64) -> Vec<u8> {
        let mut v = cbor_uint(n);
        v[0] |= 0xc0;
        v
    }

    // -----------------------------------------------------------------------
    // Position tracking
    // -----------------------------------------------------------------------

    #[test]
    fn position_advances_with_reads() {
        let data = cbor_uint(42);
        let mut r = Reader::new(&data);
        assert_eq!(r.position(), 0);
        r.read_uint().unwrap();
        assert_eq!(r.position(), data.len());
    }

    #[test]
    fn slice_from_captures_bytes() {
        let mut data = cbor_bytes(b"hello");
        data.extend_from_slice(&cbor_bytes(b"world"));
        let mut r = Reader::new(&data);
        let start = r.position();
        r.read_bytes().unwrap();
        let slice = r.slice_from(start);
        assert_eq!(slice, &cbor_bytes(b"hello"));
    }

    // -----------------------------------------------------------------------
    // Probe methods (non-advancing)
    // -----------------------------------------------------------------------

    #[test]
    fn probe_array_len_definite() {
        let data = {
            let mut v = cbor_array_hdr(3);
            v.extend(cbor_uint(1));
            v.extend(cbor_uint(2));
            v.extend(cbor_uint(3));
            v
        };
        let mut r = Reader::new(&data);
        let before = r.position();
        assert_eq!(r.probe_array_len().unwrap(), Some(3));
        // Must not advance.
        assert_eq!(r.position(), before);
    }

    #[test]
    fn probe_array_len_indefinite() {
        // 0x9f = indefinite array open
        let data = [0x9f, 0x01, 0x02, CBOR_BREAK];
        let mut r = Reader::new(&data);
        let before = r.position();
        assert_eq!(r.probe_array_len().unwrap(), None);
        assert_eq!(r.position(), before);
    }

    #[test]
    fn probe_map_len_definite() {
        let data = {
            let mut v = cbor_map_hdr(1);
            v.extend(cbor_uint(0));
            v.extend(cbor_uint(99));
            v
        };
        let mut r = Reader::new(&data);
        assert_eq!(r.probe_map_len().unwrap(), Some(1));
        assert_eq!(r.position(), 0);
    }

    // -----------------------------------------------------------------------
    // read_uint / read_int
    // -----------------------------------------------------------------------

    #[test]
    fn read_uint_small() {
        let data = cbor_uint(23);
        let mut r = Reader::new(&data);
        assert_eq!(r.read_uint().unwrap(), 23);
    }

    #[test]
    fn read_uint_large() {
        let data = cbor_uint(u64::MAX);
        let mut r = Reader::new(&data);
        assert_eq!(r.read_uint().unwrap(), u64::MAX);
    }

    #[test]
    fn read_int_positive() {
        let data = cbor_uint(100);
        let mut r = Reader::new(&data);
        assert_eq!(r.read_int().unwrap(), 100_i128);
    }

    #[test]
    fn read_int_negative() {
        // -1 encodes as 0x20, -100 encodes as cbor_neg(99)
        let data = cbor_neg(99); // -1 - 99 = -100
        let mut r = Reader::new(&data);
        assert_eq!(r.read_int().unwrap(), -100_i128);
    }

    #[test]
    fn read_int_minus_one() {
        let data = [0x20u8]; // major 1, info 0 → -1
        let mut r = Reader::new(&data);
        assert_eq!(r.read_int().unwrap(), -1_i128);
    }

    // -----------------------------------------------------------------------
    // read_bigint
    // -----------------------------------------------------------------------

    #[test]
    fn read_bigint_small_uint() {
        let data = cbor_uint(42);
        let mut r = Reader::new(&data);
        assert_eq!(r.read_bigint().unwrap(), BigInt::from(42));
    }

    #[test]
    fn read_bigint_negative_int() {
        let data = cbor_neg(0); // -1
        let mut r = Reader::new(&data);
        assert_eq!(r.read_bigint().unwrap(), BigInt::from(-1i64));
    }

    #[test]
    fn read_bigint_tag2_positive() {
        // tag(2) + bytes([0x01, 0x00]) = 256
        let mut data = cbor_tag(2);
        data.extend(cbor_bytes(&[0x01, 0x00]));
        let mut r = Reader::new(&data);
        assert_eq!(r.read_bigint().unwrap(), BigInt::from(256u64));
    }

    #[test]
    fn read_bigint_tag3_negative() {
        // tag(3) + bytes([0x00]) = -1 - 0 = -1
        let mut data = cbor_tag(3);
        data.extend(cbor_bytes(&[0x00]));
        let mut r = Reader::new(&data);
        assert_eq!(r.read_bigint().unwrap(), BigInt::from(-1i64));
    }

    #[test]
    fn read_bigint_tag2_large() {
        // tag(2) + bytes representing 2^64
        // 2^64 = 0x10000000000000000 — 9 bytes big-endian: [0x01, 0x00..0x00]
        let bytes = {
            let mut v = vec![0x01u8];
            v.extend_from_slice(&[0x00; 8]);
            v
        };
        let mut data = cbor_tag(2);
        data.extend(cbor_bytes(&bytes));
        let mut r = Reader::new(&data);
        let expected = BigInt::from(1u64) << 64;
        assert_eq!(r.read_bigint().unwrap(), expected);
    }

    #[test]
    fn read_bigint_tag3_large() {
        // tag(3) + bytes([0x01, 0x00..0x00]) = -1 - 2^64 = -(2^64 + 1)
        let bytes = {
            let mut v = vec![0x01u8];
            v.extend_from_slice(&[0x00; 8]);
            v
        };
        let mut data = cbor_tag(3);
        data.extend(cbor_bytes(&bytes));
        let mut r = Reader::new(&data);
        // -(2^64) - 1 = -(2^64 + 1), the value encoded by tag(3) || bytes(0x01,0,...,0).
        let shifted: BigInt = BigInt::from(1u64) << 64u32;
        let expected: BigInt = -shifted - BigInt::from(1u64);
        assert_eq!(r.read_bigint().unwrap(), expected);
    }

    #[test]
    fn read_bigint_zero_tag2() {
        // tag(2) + bytes([]) = 0 (empty byte string)
        let mut data = cbor_tag(2);
        data.extend(cbor_bytes(&[]));
        let mut r = Reader::new(&data);
        assert_eq!(r.read_bigint().unwrap(), BigInt::from(0u64));
    }

    // -----------------------------------------------------------------------
    // read_rational
    // -----------------------------------------------------------------------

    #[test]
    fn read_rational_tag30() {
        // tag(30) [3, 4] → 3/4
        let mut data = cbor_tag(30);
        data.extend(cbor_array_hdr(2));
        data.extend(cbor_uint(3));
        data.extend(cbor_uint(4));
        let mut r = Reader::new(&data);
        let rat = r.read_rational().unwrap();
        assert_eq!(rat.numerator, 3);
        assert_eq!(rat.denominator, 4);
    }

    #[test]
    fn read_rational_zero_denominator_rejected() {
        let mut data = cbor_tag(30);
        data.extend(cbor_array_hdr(2));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(0));
        let mut r = Reader::new(&data);
        assert!(r.read_rational().is_err());
    }

    #[test]
    fn read_rational_wrong_tag_rejected() {
        // tag(29) instead of 30
        let mut data = cbor_tag(29);
        data.extend(cbor_array_hdr(2));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(2));
        let mut r = Reader::new(&data);
        assert!(r.read_rational().is_err());
    }

    // -----------------------------------------------------------------------
    // read_bool / read_null
    // -----------------------------------------------------------------------

    #[test]
    fn read_bool_true() {
        let mut r = Reader::new(&[CBOR_TRUE]);
        assert!(r.read_bool().unwrap());
    }

    #[test]
    fn read_bool_false() {
        let mut r = Reader::new(&[CBOR_FALSE]);
        assert!(!r.read_bool().unwrap());
    }

    #[test]
    fn read_null_ok() {
        let mut r = Reader::new(&[CBOR_NULL]);
        r.read_null().unwrap();
    }

    #[test]
    fn read_null_wrong_type_rejected() {
        let data = cbor_uint(0);
        let mut r = Reader::new(&data);
        assert!(r.read_null().is_err());
    }

    // -----------------------------------------------------------------------
    // expect_tag
    // -----------------------------------------------------------------------

    #[test]
    fn expect_tag_correct() {
        let data = cbor_tag(24);
        let mut r = Reader::new(&data);
        r.expect_tag(24).unwrap();
    }

    #[test]
    fn expect_tag_wrong() {
        let data = cbor_tag(24);
        let mut r = Reader::new(&data);
        assert!(r.expect_tag(42).is_err());
    }

    // -----------------------------------------------------------------------
    // read_bytes / read_bytes_owned / read_indef_bytes
    // -----------------------------------------------------------------------

    #[test]
    fn read_bytes_ok() {
        let payload = b"cardano";
        let data = cbor_bytes(payload);
        let mut r = Reader::new(&data);
        assert_eq!(r.read_bytes().unwrap(), payload);
    }

    #[test]
    fn read_bytes_zero_length() {
        let data = cbor_bytes(&[]);
        let mut r = Reader::new(&data);
        let empty: &[u8] = &[];
        assert_eq!(r.read_bytes().unwrap(), empty);
    }

    #[test]
    fn read_indef_bytes_two_chunks() {
        // 0x5f = indefinite bytes; then two chunks; 0xff = break
        let mut data = vec![0x5f];
        data.extend(cbor_bytes(b"foo"));
        data.extend(cbor_bytes(b"bar"));
        data.push(CBOR_BREAK);
        let mut r = Reader::new(&data);
        let out = r.read_indef_bytes().unwrap();
        assert_eq!(out, b"foobar");
    }

    #[test]
    fn read_indef_bytes_empty() {
        // 0x5f 0xff = indefinite bytes with no chunks
        let data = [0x5f, CBOR_BREAK];
        let mut r = Reader::new(&data);
        let out: Vec<u8> = r.read_indef_bytes().unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn read_indef_bytes_single_chunk() {
        let mut data = vec![0x5f];
        data.extend(cbor_bytes(b"hello"));
        data.push(CBOR_BREAK);
        let mut r = Reader::new(&data);
        let out = r.read_indef_bytes().unwrap();
        assert_eq!(out, b"hello");
    }

    #[test]
    fn read_indef_bytes_falls_back_to_definite() {
        // A definite byte string passed to read_indef_bytes should still work.
        let data = cbor_bytes(b"test");
        let mut r = Reader::new(&data);
        let out = r.read_indef_bytes().unwrap();
        assert_eq!(out, b"test");
    }

    #[test]
    fn read_bytes_owned_definite() {
        let data = cbor_bytes(b"owned");
        let mut r = Reader::new(&data);
        let out = r.read_bytes_owned().unwrap();
        assert_eq!(out, b"owned");
    }

    #[test]
    fn read_bytes_owned_indef() {
        let mut data = vec![0x5f];
        data.extend(cbor_bytes(b"a"));
        data.extend(cbor_bytes(b"b"));
        data.push(CBOR_BREAK);
        let mut r = Reader::new(&data);
        let out = r.read_bytes_owned().unwrap();
        assert_eq!(out, b"ab");
    }

    // -----------------------------------------------------------------------
    // read_set — tag-258 awareness
    // -----------------------------------------------------------------------

    #[test]
    fn read_set_untagged() {
        // Plain array [1, 2, 3]
        let mut data = cbor_array_hdr(3);
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(3));
        let mut r = Reader::new(&data);
        let set = r.read_set(|rr| rr.read_uint()).unwrap();
        assert_eq!(set, vec![1, 2, 3]);
    }

    #[test]
    fn read_set_tagged_258() {
        // tag(258) [10, 20]
        let mut data = TAG_258_HEADER.to_vec();
        data.extend(cbor_array_hdr(2));
        data.extend(cbor_uint(10));
        data.extend(cbor_uint(20));
        let mut r = Reader::new(&data);
        let set = r.read_set(|rr| rr.read_uint()).unwrap();
        assert_eq!(set, vec![10, 20]);
    }

    #[test]
    fn read_set_tagged_258_empty() {
        // tag(258) []
        let mut data = TAG_258_HEADER.to_vec();
        data.extend(cbor_array_hdr(0));
        let mut r = Reader::new(&data);
        let set: Vec<u64> = r.read_set(|rr| rr.read_uint()).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn read_set_position_after_tagged() {
        // After reading a tagged set, position should be past all bytes.
        let mut data = TAG_258_HEADER.to_vec();
        data.extend(cbor_array_hdr(1));
        data.extend(cbor_uint(99));
        let total = data.len();
        let mut r = Reader::new(&data);
        r.read_set(|rr| rr.read_uint()).unwrap();
        assert_eq!(r.position(), total);
    }

    // -----------------------------------------------------------------------
    // read_set_strict — Conway PV9+ duplicate rejection
    // -----------------------------------------------------------------------

    #[test]
    fn read_set_strict_rejects_duplicate_untagged() {
        // Plain array [1, 1] — duplicate element must hard-fail at PV9+.
        let mut data = cbor_array_hdr(2);
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(1));
        let mut r = Reader::new(&data);
        let res = r.read_set_strict(|rr| rr.read_uint());
        assert!(res.is_err(), "duplicate in untagged set must be rejected");
    }

    #[test]
    fn read_set_strict_rejects_duplicate_tagged() {
        // tag(258) [1, 1] — duplicate element must hard-fail at PV9+.
        let mut data = TAG_258_HEADER.to_vec();
        data.extend(cbor_array_hdr(2));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(1));
        let mut r = Reader::new(&data);
        let res = r.read_set_strict(|rr| rr.read_uint());
        assert!(res.is_err(), "duplicate in tag-258 set must be rejected");
    }

    #[test]
    fn read_set_strict_accepts_unique() {
        // tag(258) [1, 2, 3] — all distinct, decodes Ok, preserves wire order.
        let mut data = TAG_258_HEADER.to_vec();
        data.extend(cbor_array_hdr(3));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(3));
        let mut r = Reader::new(&data);
        let set = r.read_set_strict(|rr| rr.read_uint()).unwrap();
        assert_eq!(set, vec![1, 2, 3]);
    }

    #[test]
    fn read_set_strict_accepts_unique_untagged() {
        // Plain array [3, 1, 2] — distinct, order preserved (never reordered).
        let mut data = cbor_array_hdr(3);
        data.extend(cbor_uint(3));
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(2));
        let mut r = Reader::new(&data);
        let set = r.read_set_strict(|rr| rr.read_uint()).unwrap();
        assert_eq!(set, vec![3, 1, 2]);
    }

    #[test]
    fn read_set_strict_empty() {
        // tag(258) [] — empty set decodes Ok.
        let mut data = TAG_258_HEADER.to_vec();
        data.extend(cbor_array_hdr(0));
        let mut r = Reader::new(&data);
        let set: Vec<u64> = r.read_set_strict(|rr| rr.read_uint()).unwrap();
        assert!(set.is_empty());
    }

    #[test]
    fn read_set_strict_position_after_tagged() {
        // After a successful strict read, position must be past all bytes.
        let mut data = TAG_258_HEADER.to_vec();
        data.extend(cbor_array_hdr(2));
        data.extend(cbor_uint(10));
        data.extend(cbor_uint(20));
        let total = data.len();
        let mut r = Reader::new(&data);
        r.read_set_strict(|rr| rr.read_uint()).unwrap();
        assert_eq!(r.position(), total);
    }

    #[test]
    fn read_set_strict_indefinite_rejects_duplicate() {
        // Indefinite array [_ 5, 5] — duplicate must hard-fail.
        let mut data = vec![0x9f]; // indefinite array open
        data.extend(cbor_uint(5));
        data.extend(cbor_uint(5));
        data.push(CBOR_BREAK);
        let mut r = Reader::new(&data);
        let res = r.read_set_strict(|rr| rr.read_uint());
        assert!(res.is_err(), "duplicate in indefinite set must be rejected");
    }

    #[test]
    fn read_set_strict_indefinite_accepts_unique() {
        // Indefinite array [_ 5, 6] — distinct, decodes Ok, break consumed.
        let mut data = vec![0x9f]; // indefinite array open
        data.extend(cbor_uint(5));
        data.extend(cbor_uint(6));
        data.push(CBOR_BREAK);
        let total = data.len();
        let mut r = Reader::new(&data);
        let set = r.read_set_strict(|rr| rr.read_uint()).unwrap();
        assert_eq!(set, vec![5, 6]);
        assert_eq!(r.position(), total);
    }

    // -----------------------------------------------------------------------
    // read_array — definite and indefinite
    // -----------------------------------------------------------------------

    #[test]
    fn read_array_definite() {
        let mut data = cbor_array_hdr(2);
        data.extend(cbor_uint(7));
        data.extend(cbor_uint(8));
        let mut r = Reader::new(&data);
        let arr = r.read_array(|rr| rr.read_uint()).unwrap();
        assert_eq!(arr, vec![7, 8]);
    }

    #[test]
    fn read_array_indefinite() {
        let mut data = vec![0x9f]; // indefinite array
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(2));
        data.push(CBOR_BREAK);
        let mut r = Reader::new(&data);
        let arr = r.read_array(|rr| rr.read_uint()).unwrap();
        assert_eq!(arr, vec![1, 2]);
    }

    #[test]
    fn read_array_empty() {
        let data = cbor_array_hdr(0);
        let mut r = Reader::new(&data);
        let arr: Vec<u64> = r.read_array(|rr| rr.read_uint()).unwrap();
        assert!(arr.is_empty());
    }

    // -----------------------------------------------------------------------
    // read_map — definite and indefinite
    // -----------------------------------------------------------------------

    #[test]
    fn read_map_definite() {
        let mut data = cbor_map_hdr(2);
        data.extend(cbor_uint(1));
        data.extend(cbor_uint(100));
        data.extend(cbor_uint(2));
        data.extend(cbor_uint(200));
        let mut r = Reader::new(&data);
        let m = r
            .read_map(|rr| rr.read_uint(), |rr| rr.read_uint())
            .unwrap();
        assert_eq!(m, vec![(1, 100), (2, 200)]);
    }

    #[test]
    fn read_map_indefinite() {
        // 0xbf = indefinite map
        let mut data = vec![0xbf];
        data.extend(cbor_uint(10));
        data.extend(cbor_uint(99));
        data.push(CBOR_BREAK);
        let mut r = Reader::new(&data);
        let m = r
            .read_map(|rr| rr.read_uint(), |rr| rr.read_uint())
            .unwrap();
        assert_eq!(m, vec![(10, 99)]);
    }

    // -----------------------------------------------------------------------
    // skip
    // -----------------------------------------------------------------------

    #[test]
    fn skip_uint() {
        let mut data = cbor_uint(12345);
        let skipped_len = data.len();
        data.extend(cbor_uint(42)); // sentinel
        let mut r = Reader::new(&data);
        r.skip().unwrap();
        assert_eq!(r.position(), skipped_len);
        // Sentinel should be readable.
        assert_eq!(r.read_uint().unwrap(), 42);
    }

    #[test]
    fn skip_nested_array() {
        // [[1, 2], 3]
        let mut inner = cbor_array_hdr(2);
        inner.extend(cbor_uint(1));
        inner.extend(cbor_uint(2));
        let mut data = cbor_array_hdr(2);
        data.extend(&inner);
        data.extend(cbor_uint(3));
        let mut r = Reader::new(&data);
        r.skip().unwrap(); // skip the whole outer array
        assert_eq!(r.position(), data.len());
    }

    #[test]
    fn skip_tagged_value() {
        // tag(24) bytes([0xab, 0xcd])
        let mut data = cbor_tag(24);
        data.extend(cbor_bytes(&[0xab, 0xcd]));
        let total = data.len();
        let mut r = Reader::new(&data);
        r.skip().unwrap();
        assert_eq!(r.position(), total);
    }

    // -----------------------------------------------------------------------
    // read_embedded_cbor_bytes
    // -----------------------------------------------------------------------

    #[test]
    fn read_embedded_cbor_bytes_ok() {
        // tag(24) + bytes([0x01]) — inner CBOR is just uint(1)
        let inner_cbor = [0x01u8];
        let mut data = cbor_tag(24);
        data.extend(cbor_bytes(&inner_cbor));
        let mut r = Reader::new(&data);
        let result = r.read_embedded_cbor_bytes().unwrap();
        assert_eq!(result, &inner_cbor);
    }

    // -----------------------------------------------------------------------
    // Bounded integer decoding (overflow safety)
    // -----------------------------------------------------------------------

    #[test]
    fn read_bigint_u64_max_via_tag2() {
        // tag(2) + bytes(big-endian u64::MAX) — should decode fine as BigInt
        let bytes = u64::MAX.to_be_bytes();
        let mut data = cbor_tag(2);
        data.extend(cbor_bytes(&bytes));
        let mut r = Reader::new(&data);
        let result = r.read_bigint().unwrap();
        assert_eq!(result, BigInt::from(u64::MAX));
    }

    // -----------------------------------------------------------------------
    // Multiple sequential reads — position consistency
    // -----------------------------------------------------------------------

    #[test]
    fn sequential_reads_correct_position() {
        let mut data = cbor_uint(1);
        data.extend(cbor_uint(2));
        data.extend(cbor_bytes(b"x"));
        let mut r = Reader::new(&data);
        let p0 = r.position();
        r.read_uint().unwrap();
        let p1 = r.position();
        r.read_uint().unwrap();
        let p2 = r.position();
        r.read_bytes().unwrap();
        let p3 = r.position();
        assert!(p0 < p1);
        assert!(p1 < p2);
        assert!(p2 < p3);
        assert_eq!(p3, data.len());
    }
}
