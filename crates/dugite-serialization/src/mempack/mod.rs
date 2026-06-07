//! MemPack UTxO decoder for Haskell ledger state InMemory tables blobs.
//!
//! Mithril ancillary archives ship the InMemory backend's UTxO set as a single
//! MemPack-encoded blob. The on-disk location depends on the
//! `ouroboros-consensus` version that produced the snapshot:
//!
//! * `< 1.0.0.0` (cardano-node `<= 10.6.x`): `<snap>/tables/tvar`
//! * `>= 1.0.0.0` (cardano-node `>= 11.0.1`): `<snap>/tables` (flat file)
//!
//! The decoder itself is agnostic to which path the bytes came from. Path
//! resolution is handled by the importer (see
//! `dugite_node::node::resolve_inmemory_tables_path`). See issue #460.
//!
//! The blob contents are serialised as:
//!
//! ```text
//! array(1) [
//!   map(indefinite) {        // 0xbf … 0xff
//!     bytes(34) → bytes(N),  // MemPack TxIn → MemPack TxOut
//!     …
//!   }
//! ]
//! ```
//!
//! ## TxIn encoding (34-byte key)
//!
//! ```text
//! TxId (32 bytes, big-endian) ‖ TxIx (2 bytes, endianness is snapshot-dependent)
//! ```
//!
//! ### TxIx endianness is decided AUTHORITATIVELY by the snapshot codec version
//!
//! There are two on-disk `TxIx` byte orders, and they are **NOT** correlated
//! with the flat-vs-nested filesystem layout. The two upstream changes landed in
//! *different* `ouroboros-consensus` releases:
//!
//! * The flat-`tables` / `SerializeTablesWithHint` layout landed ~oc 0.25.0.0
//!   (Apr 2025), and the blob later *moved* from `<snap>/tables` to
//!   `<snap>/tables/<snap>` (commit `286ad7ec8`, Oct 2025).
//! * The big-endian `TxIx` flip — a `BigEndianTxIn` newtype whose `MemPack`
//!   instance byte-swaps the index (`packM (byteSwap16 w)`, commit `9ac9388`,
//!   "Flip TxIx serialization to big endian") — landed *independently*.
//!
//! A layout→endianness mapping therefore mis-keys (index 1 ⇄ 256), and the BE
//! flip added no version byte / magic to the `tables` blob itself (a flat-LE blob
//! is byte-identical to a flat-BE blob), so the byte order is NOT recoverable
//! from blob content or filesystem layout.
//!
//! The byte order IS, however, recoverable from the snapshot's **codec version**,
//! which upstream records in the snapshot's sibling `meta` JSON file
//! (`snapshotTablesCodecVersion :: TablesCodecVersion`). This is the authoritative
//! signal and the engine's decision-maker. The upstream type maps version → byte
//! order EXACTLY (`Ouroboros.Consensus.Storage.LedgerDB.Snapshots`):
//!
//! ```haskell
//! data TablesCodecVersion
//!   = -- | Used in cardano-node 10.7. Previous versions have no codec version.
//!     -- [ {_ (txid, big-endian txix) => txout} ]
//!     TablesCodecVersion1
//!   deriving (Eq, Show)
//!
//! instance ToJSON TablesCodecVersion where
//!   toJSON TablesCodecVersion1 = Aeson.Number 1
//!
//! instance FromJSON SnapshotMetadata where
//!   parseJSON = withObject "SnapshotMetadata" $ \o -> SnapshotMetadata
//!     <$> o .: "backend"
//!     <*> o .: "checksum"
//!     <*> o .: "tablesCodecVersion"    -- MANDATORY .: (absent/null => Aeson Left)
//! instance FromJSON TablesCodecVersion where
//!   parseJSON v = enforceVersion =<< parseJSON v
//! enforceVersion :: Word8 -> Parser TablesCodecVersion
//! enforceVersion v = case v of
//!   1 -> pure TablesCodecVersion1
//!   _ -> fail "Unknown or outdated tables codec version"
//! ```
//!
//! ENDIANNESS/BACKEND/VERSION DECISION (#10). The byte-order / backend / codec-
//! version decision below is byte-exact with upstream `FromJSON SnapshotMetadata`,
//! `FromJSON TablesCodecVersion` / `enforceVersion`, and the V2/InMemory backend
//! guard (see [`TxIxEndianness::from_tables_codec_version`]). SCOPE NOTE: this
//! import path performs ONLY that endianness/backend/version decision — it does
//! NOT verify the snapshot's CRC/checksum integrity (upstream `loadSnapshot`
//! checks `crcOfConcat == snapshotChecksum`); that integrity check is tracked
//! separately as #17. The version→endianness mapping is:
//!
//! * `tablesCodecVersion == 1` (`TablesCodecVersion1`) ⇒ **big-endian** txix
//!   (the Haddock literally documents the layout as `(txid, big-endian txix)`).
//!   This is the ONLY accepted version and the only path that ever selects an
//!   endianness — chain-verified against the modern preprod mithril snapshot.
//! * a present meta whose `tablesCodecVersion` is ABSENT or null ⇒ `MetadataInvalid`
//!   ⇒ HARD ERROR (the mandatory `o .: "tablesCodecVersion"` is an Aeson `Left`),
//!   in BOTH the offline converter (`getMetadata: MetadataInvalid -> throwError`)
//!   and the node loader (`V2/InMemory.hs` ⇒ `ReadMetadataError`).
//! * a MISSING meta FILE ⇒ HARD ERROR (`ReadMetadataError`). `getMetadata`'s
//!   `MetadataFileDoesNotExist -> Nothing` is the CRC-SKIP path, NOT a decode-LE
//!   branch — endianness is NEVER selected from a missing/absent version.
//! * any other numeric value ⇒ **unknown** ⇒ ERROR (`enforceVersion` rejects
//!   everything but `1`). We NEVER guess and NEVER silently default to LE — the
//!   cardinal rule (default to rejection / byte-exact only). `Big` is the only
//!   byte order the importer can ever select.
//!
//! [`cross_validate_txix_endianness`] and [`assert_txix_distribution_sane`] are
//! NOT the import decider (the codec version is), but they ARE live on the import
//! path as defense-in-depth vetoes: the node calls `cross_validate_txix_endianness`
//! right after deriving the byte order from the version, and `assert_txix_distribution_sane`
//! after decoding the table, so a meta whose version contradicts the actual UTxO
//! index distribution is rejected loudly rather than silently mis-keyed. The
//! empirical [`detect_txix_endianness`] is the only one reserved for tests /
//! fixtures that ship no `meta` file. The version decides and only ever yields `Big`.
//!
//! Canonical Haskell `MemPack` instances:
//! ```haskell
//! -- LEGACY: Cardano.Ledger.TxIn / Cardano.Ledger.BaseTypes
//! instance MemPack TxIn where packM (TxIn txId txIx) = packM txId >> packM txIx
//! newtype TxIx = TxIx Word16 deriving newtype (… MemPack) -- host-native LE
//! -- generic Word16 MemPack (lehins/mempack) is host-native:
//! --   packM a@(W16# a#) = … writeWord8ArrayAsWord16# mba# i# a#  -- no byteswap
//! -- NEW (BigEndianTxIn): Ouroboros.Consensus.Shelley.Ledger.Ledger
//! instance MemPack BigEndianTxIx where
//!   packM (BigEndianTxIx (SL.TxIx w)) = packM (byteSwap16 w)
//! ```
//!
//! ## TxOut encoding (value blob)
//!
//! The first byte is a tag (0–5) selecting a Haskell constructor variant.
//! See [`txout::decode_mempack_txout`] for the per-variant layout.

pub mod compact;
pub mod txout;

#[cfg(test)]
mod tests;

use crate::error::SerializationError;
use crate::haskell_snapshot::cbor_utils::{decode_array_len, decode_bytes};
use dugite_primitives::hash::Hash;

/// On-disk byte order of the 2-byte `TxIx` trailer in a MemPack TxIn key.
///
/// This is **not** discoverable from the filesystem layout, nor from the blob
/// bytes — a flat-LE blob is byte-identical to a flat-BE blob. It IS determined
/// authoritatively from the snapshot's codec version
/// (`snapshotTablesCodecVersion`, recorded in the sibling `meta` JSON file) via
/// [`TxIxEndianness::from_tables_codec_version`], which under STRICT semantics
/// only ever yields [`TxIxEndianness::Big`] for a real import (`Some(1)`); every
/// other meta outcome is a hard error. [`cross_validate_txix_endianness`] and
/// [`assert_txix_distribution_sane`] are LIVE defense-in-depth vetoes on the
/// import path (they reject a version that contradicts the data); the empirical
/// [`detect_txix_endianness`] is for tests / fixtures with no `meta` file. See
/// the module docs for the canonical Haskell sources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TxIxEndianness {
    /// Host-native little-endian `Word16` (legacy `MemPack TxIn`, pre-`BigEndianTxIn`).
    ///
    /// NOTE (#10 strict): this variant is NEVER selected by the import decision —
    /// `from_tables_codec_version` only returns `Big`. It IS used by the live
    /// cross-validator / safety-net helpers (which decode under both byte orders
    /// to refute a wrong version) and by tests that decode legacy LE blobs
    /// directly; it is not reachable from a snapshot's codec version.
    ///
    /// On-disk `01 00` => index 1. Read with `from_le_bytes`.
    Little,
    /// Big-endian `Word16` (`BigEndianTxIn` byte-swaps the index; codec version 1
    /// — `TablesCodecVersion1`, documented as `(txid, big-endian txix)`).
    ///
    /// On-disk `00 01` => index 1. Read with `from_be_bytes`.
    Big,
}

impl TxIxEndianness {
    /// Map a snapshot's `tablesCodecVersion` to the authoritative on-disk `TxIx`
    /// byte order. STRICT: collapses to `{Some(1) => Big, else => Err}`. `None`
    /// is UNREPRESENTABLE from any accepted meta.
    ///
    /// STRICT SEMANTICS (#10). Byte-exact with upstream `FromJSON SnapshotMetadata`
    /// and `enforceVersion`; this maps an accepted Word8 to a byte order only and
    /// does NOT cover snapshot CRC/checksum integrity (tracked as #17).
    ///
    /// Upstream `FromJSON SnapshotMetadata` (`LedgerDB/Snapshots.hs`) reads the
    /// codec version with the MANDATORY `o .: "tablesCodecVersion"`, then feeds
    /// it through `enforceVersion`. A meta that parses as JSON but LACKS the
    /// field (or has it null) ⇒ Aeson `Left` ⇒ `MetadataInvalid` ⇒ HARD ERROR —
    /// in BOTH the offline converter (`getMetadata: MetadataInvalid why ->
    /// throwError`) and the node loader (`V2/InMemory.hs` ⇒ `ReadMetadataError`).
    /// And `getMetadata`'s `MetadataFileDoesNotExist -> Nothing` is the CRC
    /// SKIP path — endianness is NEVER selected from a missing/absent version.
    ///
    /// ```haskell
    /// -- Ouroboros.Consensus.Storage.LedgerDB.Snapshots
    /// instance FromJSON SnapshotMetadata where
    ///   parseJSON = withObject "SnapshotMetadata" $ \o -> SnapshotMetadata
    ///     <$> o .: "backend"
    ///     <*> o .: "checksum"
    ///     <*> o .: "tablesCodecVersion"        -- MANDATORY .: (absent/null => Left)
    /// instance FromJSON TablesCodecVersion where parseJSON v = enforceVersion =<< parseJSON v
    /// enforceVersion v = case v of { 1 -> pure TablesCodecVersion1
    ///                              ; _ -> fail "Unknown or outdated tables codec version" }
    /// ```
    ///
    /// * `Some(1)` ⇒ [`TxIxEndianness::Big`]  (`TablesCodecVersion1` — big-endian
    ///   txix; the ONLY accepted version, chain-verified against preprod).
    /// * `None` and any other value ⇒ `Err`. `None` is unrepresentable from any
    ///   accepted meta — a present meta with an absent/null field is
    ///   `MetadataInvalid`; a missing meta FILE is `ReadMetadataError`; neither
    ///   reaches a "decode little-endian" branch. We reject rather than silently
    ///   default to LE (byte-exact only / default to rejection). NEVER guess.
    pub fn from_tables_codec_version(version: Option<u64>) -> Result<Self, SerializationError> {
        match version {
            // TablesCodecVersion1 — Haddock: "[ {_ (txid, big-endian txix) => txout} ]".
            // The ONLY version upstream `enforceVersion` accepts.
            Some(1) => Ok(TxIxEndianness::Big),
            // `None` is unrepresentable from any accepted meta: a present meta with
            // an absent/null field is `MetadataInvalid`; a missing meta file is
            // `ReadMetadataError`. Reject — no silent legacy little-endian fallback.
            None => Err(SerializationError::CborDecode(
                "snapshot meta has no tablesCodecVersion (MetadataInvalid upstream: the \
                 mandatory `o .: \"tablesCodecVersion\"` fails); refusing to import a snapshot \
                 without a valid tables codec version (endianness is never selected from a \
                 missing/absent version — no silent little-endian fallback)"
                    .to_string(),
            )),
            Some(other) => Err(SerializationError::CborDecode(format!(
                "snapshot meta tablesCodecVersion={other} is unknown/outdated (upstream \
                 enforceVersion accepts only 1 => TablesCodecVersion1); refusing to import \
                 (cannot determine TxIx endianness without guessing)"
            ))),
        }
    }
}

/// Parse the `tablesCodecVersion` field out of a snapshot `meta` JSON blob.
///
/// The `meta` file sits next to `state`/`tables` and is the small JSON object
/// upstream writes for every snapshot, e.g.
/// `{"backend":"utxohd-mem","checksum":2409556997,"tablesCodecVersion":1}`.
///
/// STRICT (#10, re-gauntlet w4007sv2k). Upstream `FromJSON SnapshotMetadata`
/// reads the field with the MANDATORY `o .: "tablesCodecVersion"`: a present
/// meta that parses as JSON but LACKS the field (or has it null) is an Aeson
/// `Left` ⇒ `MetadataInvalid` ⇒ HARD ERROR. We mirror that exactly — there is
/// no `Ok(None)` for an absent/null field; the return is `Some(v)` or an error.
///
/// Returns:
/// * `Ok(v)` when a numeric `tablesCodecVersion` is present and Aeson's
///   `FromJSON Word8` (`Scientific.toBoundedInteger`) would accept it — i.e. any
///   JSON `Number` that is INTEGRAL (no fractional part) and within Word8
///   `[0, 255]`. The float-syntax integral forms `1.0` / `1e0` / `100e-2` are
///   ACCEPTED here (they normalise to `1`), matching Aeson byte-for-byte;
/// * `Err` when the JSON is invalid; when `tablesCodecVersion` is absent or null
///   (`MetadataInvalid` — the mandatory `.:` fails); or when it is present but is
///   a JSON string (`"1"`), a non-integral number (`1.5`), or out of Word8 range
///   (`256` / `-1`) — exactly the cases `Scientific.toBoundedInteger @Word8`
///   rejects (refuse rather than guess).
///
/// The caller feeds the returned `v` to [`TxIxEndianness::from_tables_codec_version`],
/// whose `enforceVersion` then accepts only `1` => `TablesCodecVersion1`.
pub fn parse_tables_codec_version(meta_json: &[u8]) -> Result<u64, SerializationError> {
    // AESON-FAITHFUL DUPLICATE-KEY RESOLUTION (#10 round-4 F1).
    //
    // `loadSnapshotMetadata` reads the meta with `Aeson.eitherDecode`, which is the
    // DEFAULT `json` parser. On a DUPLICATE object key, aeson's default keeps the
    // FIRST occurrence (verbatim haddock, aeson-2.2 `Data.Aeson.Parser.Internal`):
    //
    // ```haskell
    // -- | Parse any JSON value. Synonym of 'json'.
    // value :: Parser Value
    // value = jsonWith (pure . KM.fromList)
    //
    // -- 'json' keeps only the first occurrence of each key, using
    // -- 'Data.Aeson.KeyMap.fromList'.
    // --   'json' = 'jsonWith' ('Right' . 'H.fromList')
    // -- 'jsonLast' keeps the last occurrence of each key, using
    // --   @'HashMap.Lazy.fromListWith' ('const' 'id')@.
    // jsonLast :: Parser Value
    // jsonLast = jsonWith (Right . KM.fromListWith (const id))
    //
    // -- Ouroboros.Consensus.Storage.LedgerDB.Snapshots
    // loadSnapshotMetadata … = … case Aeson.eitherDecode bs of
    //   Left decodeErr -> pure $ Left $ MetadataInvalid decodeErr
    //   Right meta -> pure $ Right meta
    // instance FromJSON SnapshotMetadata where
    //   parseJSON = Aeson.withObject "SnapshotMetadata" $ \o ->
    //     SnapshotMetadata <$> o .: "backend" <*> fmap CRC (o .: "checksum")
    //                      <*> o .: "tablesCodecVersion"
    // ```
    //
    // `serde_json` keeps the LAST occurrence (its `Map::insert` overwrites), so a
    // `serde_json::Value`-based classification would DISAGREE with aeson. We therefore
    // drive BOTH the null/type/number GATE ([`first_occurrence_value`]) AND the raw
    // literal VALUE ([`top_level_number_literal`]) from the SAME top-level,
    // first-occurrence resolution: a `MapAccess` visitor / structure-aware walk that
    // captures the first TOP-LEVEL `tablesCodecVersion` and ignores every later
    // duplicate and every nested same-named key, exactly like `KM.fromList` +
    // `KM.lookup`. For the common (non-duplicate, top-level) case the behaviour is
    // byte-identical to the previous `serde_json` classification.
    let value: serde_json::Value = serde_json::from_slice(meta_json).map_err(|e| {
        SerializationError::CborDecode(format!("snapshot meta is not valid JSON: {e}"))
    })?;
    // Ensure the top-level is an object (aeson `withObject "SnapshotMetadata"`); a
    // non-object meta fails the same way upstream does.
    if !value.is_object() {
        return Err(SerializationError::CborDecode(
            "snapshot meta is not a JSON object (aeson `withObject \"SnapshotMetadata\"` \
             fails); refusing to import"
                .to_string(),
        ));
    }
    // First-occurrence value of `tablesCodecVersion`, aeson `KM.fromList` semantics.
    let first = first_occurrence_value(meta_json, "tablesCodecVersion")?;
    match first {
        // Absent or null => MetadataInvalid upstream (mandatory `o .: "tablesCodecVersion"`
        // is an Aeson `Left`). Reject rather than silently default to legacy LE.
        None | Some(serde_json::Value::Null) => Err(SerializationError::CborDecode(
            "snapshot meta has no tablesCodecVersion field (MetadataInvalid upstream: the \
             mandatory `o .: \"tablesCodecVersion\"` fails on an absent/null field); refusing \
             to import — no silent little-endian fallback"
                .to_string(),
        )),
        // A JSON string (`"1"`), bool, array, or object is NOT a `Number`. Upstream
        // `withScientific' f v = case v of { Number n -> f n; _ -> typeMismatch "Number" v }`
        // ⇒ Aeson FAILS before the coercion. Reject (no guessing).
        Some(v) if !v.is_number() => Err(SerializationError::CborDecode(format!(
            "snapshot meta tablesCodecVersion is not a JSON Number (Aeson `withScientific'` \
             yields `typeMismatch \"Number\"` for {v}); refusing to import"
        ))),
        // A JSON `Number`. We must apply the Aeson `Scientific.toBoundedInteger @Word8`
        // integral+range test on the EXACT decimal/scientific literal. `serde_json` is
        // built WITHOUT `arbitrary_precision`, so its parsed `f64` cannot distinguish
        // `1.0` from `1.0000000000000001` (the latter rounds to `1.0` and would be
        // WRONGLY accepted). So we extract the raw number token straight from the bytes
        // and test the literal itself — never an `f64`.
        //
        // #10 round-5 R1 — the literal MUST come from the SAME top-level,
        // first-occurrence structural resolution as the gate above. Aeson's
        // `o .: "tablesCodecVersion"` reads ONLY the named TOP-LEVEL key, ignoring any
        // identically-named key nested inside a sibling's object/array value (verbatim
        // aeson-2.2 `Data.Aeson.Types.FromJSON`):
        //
        // ```haskell
        // (.:) :: FromJSON a => Object -> Key -> Parser a
        // obj .: key = case KM.lookup key obj of   -- TOP-LEVEL KeyMap lookup ONLY
        //                Nothing -> fail $ "key " ++ show key ++ " not found"
        //                Just v  -> parseJSON v <?> Key key
        // withObject name f = \v -> case v of
        //   Object obj -> f obj
        //   _          -> typeMismatch name v
        // ```
        //
        // The previous `extract_raw_number_literal` was a FLAT byte scan that matched
        // `"tablesCodecVersion"` ANYWHERE — including inside a nested object/array value
        // — so on meta like
        //   {"backend":…,"extra":{"tablesCodecVersion":99},"tablesCodecVersion":1}
        // the gate (top-level, first-wins) saw `1` but the value scan returned `99`,
        // making dugite STRICTER than aeson and rejecting a snapshot aeson loads. We
        // now read the literal from the TOP-LEVEL value span ONLY (structure-scoped),
        // so the gate and the value can never disagree.
        Some(_) => {
            let literal =
                top_level_number_literal(meta_json, "tablesCodecVersion")?.ok_or_else(|| {
                    SerializationError::CborDecode(
                        "snapshot meta tablesCodecVersion is a JSON Number but its raw literal \
                         could not be located as a top-level object value in the source bytes; \
                         refusing to import"
                            .to_string(),
                    )
                })?;
            scientific_literal_to_word8_codec_version(&literal)
        }
    }
}

/// Resolve the FIRST occurrence of the top-level object key `key` in `meta_json`,
/// matching aeson's default `json` parser (`value = jsonWith (pure . KM.fromList)`),
/// whose haddock states it "keeps only the first occurrence of each key".
///
/// `serde_json::Value` keeps the LAST duplicate (its `Map::insert` overwrites), so we
/// cannot use it to classify a duplicate key the way aeson does. Instead we run a
/// `MapAccess` visitor over the SAME bytes that captures the first value for `key` and
/// discards every later duplicate (and every other key). Returns:
/// * `Ok(None)` if the key is absent,
/// * `Ok(Some(v))` with the first occurrence's value (which may be `Null`),
/// * `Err` only if the bytes are not valid JSON or not a JSON object.
fn first_occurrence_value(
    meta_json: &[u8],
    key: &str,
) -> Result<Option<serde_json::Value>, SerializationError> {
    use serde::de::{Deserializer, MapAccess, Visitor};
    use std::fmt;

    struct FirstWins<'k> {
        key: &'k str,
    }

    impl<'de> Visitor<'de> for FirstWins<'_> {
        type Value = Option<serde_json::Value>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(f, "a JSON object")
        }

        fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            let mut found: Option<serde_json::Value> = None;
            while let Some(k) = map.next_key::<String>()? {
                let v = map.next_value::<serde_json::Value>()?;
                // aeson `KM.fromList` keeps the FIRST occurrence: only record the
                // earliest match; ignore later duplicates and unrelated keys.
                if found.is_none() && k == self.key {
                    found = Some(v);
                }
            }
            Ok(found)
        }
    }

    let mut de = serde_json::Deserializer::from_slice(meta_json);
    let result = de.deserialize_map(FirstWins { key }).map_err(|e| {
        SerializationError::CborDecode(format!(
            "snapshot meta is not a valid JSON object (aeson `withObject` fails): {e}"
        ))
    })?;
    de.end().map_err(|e| {
        SerializationError::CborDecode(format!("snapshot meta has trailing JSON garbage: {e}"))
    })?;
    Ok(result)
}

/// Return the EXACT raw source literal of the value of the FIRST TOP-LEVEL object key
/// `key`, restricted to the case where that value is a JSON number — `None` if the
/// top-level key is absent or its value is not a number token.
///
/// #10 round-5 R1 — structure-scoped replacement for the previous flat byte scan
/// on the codec-version path. This is a JSON-aware walk
/// of the TOP-LEVEL object only: it skips over every nested object/array/string value
/// (tracking brace/bracket depth and string escapes) so that an identically-named key
/// nested inside a sibling value can NEVER be matched. It therefore agrees with the
/// aeson `o .: "tablesCodecVersion"` top-level `KeyMap.lookup` and with the
/// first-occurrence gate ([`first_occurrence_value`]) byte-for-byte.
///
/// Matching aeson `KM.fromList`, the FIRST top-level occurrence of `key` wins; any
/// later duplicate top-level key is ignored (we return on the first match).
///
/// The caller has already proven (via `first_occurrence_value`) that the blob is a
/// JSON object and the first top-level `key` value is a JSON `Number`, so the only
/// `Ok(None)` paths here are belt-and-braces (e.g. the value turned out not to be a
/// bare number token); `Err` is returned only when the bytes are not UTF-8 or the
/// outer object is structurally malformed (which `from_slice` would also have caught).
fn top_level_number_literal(
    meta_json: &[u8],
    key: &str,
) -> Result<Option<String>, SerializationError> {
    let text = std::str::from_utf8(meta_json).map_err(|e| {
        SerializationError::CborDecode(format!("snapshot meta is not valid UTF-8: {e}"))
    })?;
    let bytes = text.as_bytes();
    let mut i = 0usize;
    // Skip leading whitespace, require the opening '{' of the top-level object.
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= bytes.len() || bytes[i] != b'{' {
        return Err(SerializationError::CborDecode(
            "snapshot meta top level is not a JSON object (`{` expected)".to_string(),
        ));
    }
    i += 1; // past '{'
    loop {
        // Skip whitespace and entry-separating commas between top-level members.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            return Err(SerializationError::CborDecode(
                "snapshot meta object ended without a closing `}`".to_string(),
            ));
        }
        if bytes[i] == b'}' {
            // End of the top-level object, key not found at top level.
            return Ok(None);
        }
        if bytes[i] != b'"' {
            return Err(SerializationError::CborDecode(
                "snapshot meta object member did not start with a quoted key".to_string(),
            ));
        }
        // Parse the (string) key, honouring backslash escapes.
        let (member_key, next) = parse_json_string_at(bytes, i).ok_or_else(|| {
            SerializationError::CborDecode(
                "snapshot meta object key string was unterminated".to_string(),
            )
        })?;
        i = next;
        // Skip whitespace, require ':'.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b':' {
            return Err(SerializationError::CborDecode(
                "snapshot meta object member missing `:` after key".to_string(),
            ));
        }
        i += 1; // past ':'
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value_start = i;
        let value_end = skip_json_value(bytes, value_start).ok_or_else(|| {
            SerializationError::CborDecode(
                "snapshot meta object member had a malformed value".to_string(),
            )
        })?;
        if member_key == key {
            // FIRST top-level occurrence wins (aeson `KM.fromList`). Return the raw
            // value token only if it is a bare number literal (not a string/object/etc.);
            // the caller already classified it as a Number, so anything else => None.
            let raw = &text[value_start..value_end];
            let first = raw.as_bytes().first().copied();
            let is_number_token = matches!(first, Some(b) if b == b'-' || b.is_ascii_digit());
            return Ok(if is_number_token {
                Some(raw.to_string())
            } else {
                None
            });
        }
        i = value_end;
    }
}

/// Parse a JSON string starting at `bytes[start] == b'"'`, returning the decoded
/// contents (with escapes resolved enough to compare object keys) and the index just
/// past the closing quote. `None` if the string is unterminated.
///
/// We only need to (a) find the closing quote correctly in the presence of escaped
/// quotes/backslashes, and (b) compare the key against the ASCII literal
/// `"tablesCodecVersion"`, which contains no escapes — so a faithful escape decode of
/// the common escapes is sufficient and any `\uXXXX` is passed through opaquely (it
/// can never equal the plain ASCII target key).
fn parse_json_string_at(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    debug_assert_eq!(bytes.get(start), Some(&b'"'));
    let mut i = start + 1;
    let mut out = String::new();
    while i < bytes.len() {
        let b = bytes[i];
        match b {
            b'"' => return Some((out, i + 1)),
            b'\\' => {
                i += 1;
                let e = *bytes.get(i)?;
                match e {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        // Pass the \uXXXX escape through opaquely — it can never equal a
                        // plain-ASCII key like "tablesCodecVersion", and we only use the
                        // decoded string for an equality test.
                        out.push('\\');
                        out.push('u');
                    }
                    _ => return None, // invalid escape
                }
                i += 1;
            }
            _ => {
                out.push(b as char);
                i += 1;
            }
        }
    }
    None // unterminated
}

/// Skip exactly one JSON value starting at `bytes[start]`, returning the index just
/// past the end of that value. Handles objects/arrays (nested, brace/bracket-balanced
/// with string-aware depth), strings (escape-aware), and bare scalars
/// (number/`true`/`false`/`null`). `None` on a structural error.
///
/// This is what makes [`top_level_number_literal`] structure-scoped: a nested object
/// or array value is consumed as a single unit, so a `tablesCodecVersion` key buried
/// inside it is skipped over wholesale and can never be matched at the top level.
fn skip_json_value(bytes: &[u8], start: usize) -> Option<usize> {
    let mut i = start;
    if i >= bytes.len() {
        return None;
    }
    match bytes[i] {
        b'"' => {
            // String value: reuse the escape-aware scanner.
            let (_s, end) = parse_json_string_at(bytes, i)?;
            Some(end)
        }
        b'{' | b'[' => {
            // Object or array: walk to the matching close, tracking depth and skipping
            // over nested strings (whose braces/brackets must NOT affect depth).
            let mut depth = 0i64;
            let mut in_string = false;
            let mut escaped = false;
            while i < bytes.len() {
                let b = bytes[i];
                if in_string {
                    if escaped {
                        escaped = false;
                    } else if b == b'\\' {
                        escaped = true;
                    } else if b == b'"' {
                        in_string = false;
                    }
                } else {
                    match b {
                        b'"' => in_string = true,
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(i + 1);
                            }
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
            None // unbalanced
        }
        _ => {
            // Bare scalar: number / true / false / null. Consume up to (but not
            // including) the next structural delimiter, whitespace, or end.
            let scalar_start = i;
            while i < bytes.len() {
                let b = bytes[i];
                if b.is_ascii_whitespace() || b == b',' || b == b'}' || b == b']' {
                    break;
                }
                i += 1;
            }
            if i == scalar_start {
                None
            } else {
                Some(i)
            }
        }
    }
}

/// Decide whether the EXACT JSON number literal `literal` is accepted by Aeson's
/// `FromJSON Word8`, returning its integer value as a `u64` if so.
///
/// `serde_json` in this workspace is built WITHOUT `arbitrary_precision`, so its
/// parsed `f64` collapses `1.0` and `1.0000000000000001` to the same bit pattern.
/// An `f64`-based integral test (`f.fract() == 0.0`) therefore WRONGLY accepts the
/// sub-ULP fractional `1.0000000000000001` that Aeson REJECTS. To be byte-exact we
/// run the integral+range test on the raw decimal/scientific literal — never an
/// `f64`.
///
/// Upstream `enforceVersion :: Word8 -> Parser TablesCodecVersion` runs AFTER
/// `parseJSON @Word8`, and Aeson decodes a JSON `Number` into a bounded integral
/// via `Scientific.toBoundedInteger` (verbatim, aeson-2.2 + scientific-0.3.8):
///
/// ```haskell
/// -- Data.Aeson.Types.FromJSON
/// parseBoundedIntegralFromScientific :: (Bounded a, Integral a) => Scientific -> Parser a
/// parseBoundedIntegralFromScientific s = maybe
///     (fail $ "value is either floating or will cause over or underflow " ++ show s)
///     pure
///     (Scientific.toBoundedInteger s)
/// parseBoundedIntegral :: (Bounded a, Integral a) => String -> Value -> Parser a
/// parseBoundedIntegral name =
///     prependContext name . withScientific' parseBoundedIntegralFromScientific
/// withScientific' :: (Scientific -> Parser a) -> Value -> Parser a
/// withScientific' f v = case v of
///     Number n -> f n
///     _ -> typeMismatch "Number" v
/// instance FromJSON Word8 where parseJSON = parseBoundedIntegral "Word8"
///
/// -- Data.Scientific
/// toBoundedInteger :: forall i. (Integral i, Bounded i) => Scientific -> Maybe i
/// toBoundedInteger s
///     | c == 0    = fromIntegerBounded 0
///     | integral  = if dangerouslyBig then Nothing else fromIntegerBounded n
///     | otherwise = Nothing
///   where
///     c = coefficient s
///     integral = e >= 0 || e' >= 0          -- INTEGRAL iff exponent (normalised) >= 0
///     e  = base10Exponent s
///     e' = base10Exponent (normalize s)
///     fromIntegerBounded i
///         | i < iMinBound || i > iMaxBound = Nothing   -- [minBound, maxBound]
///         | otherwise                      = Just (fromInteger i)
/// ```
///
/// So Aeson ACCEPTS a JSON `Number` iff it is INTEGRAL (zero fractional part,
/// i.e. `coefficient * 10^exponent ∈ ℤ`) AND within `Word8 [0, 255]`. The
/// float-syntax integral forms `1.0`, `1e0`, `100e-2` all normalise to the
/// integer `1` and are accepted; `1.5` / `1.0000000000000001` are non-integral
/// and rejected; `256` / `-1` are out of range and rejected. A JSON string
/// `"1"` is not a `Number` (`withScientific'` ⇒ `typeMismatch "Number"`) and is
/// rejected by the caller before reaching this function.
fn scientific_literal_to_word8_codec_version(literal: &str) -> Result<u64, SerializationError> {
    let reject = || {
        SerializationError::CborDecode(format!(
            "snapshot meta tablesCodecVersion literal `{literal}` is not an integral value in \
             Word8 range [0, 255] (Aeson Scientific.toBoundedInteger @Word8 would fail)"
        ))
    };
    scientific_literal_as_word8(literal).ok_or_else(reject)
}

/// Pure, `f64`-free evaluation of `Scientific.toBoundedInteger @Word8` over a JSON
/// number literal. Returns `Some(n)` iff the literal denotes an integer in
/// `[0, 255]` (Word8), exactly matching Aeson; `None` otherwise.
///
/// A JSON number literal is `-?digits(.digits)?([eE][+-]?digits)?`. We parse the
/// significand into an exact integer coefficient + a base-10 point position, fold
/// in the explicit exponent, and check that the resulting rational is integral and
/// in range — all with integer arithmetic, so `1.0000000000000001` (which has 16
/// fractional digits) is correctly seen as NON-integral and rejected.
fn scientific_literal_as_word8(literal: &str) -> Option<u64> {
    let s = literal.trim();
    let (negative, rest) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    // Split off an explicit exponent (`e`/`E`).
    let (mantissa, exp): (&str, i64) = match rest.find(['e', 'E']) {
        Some(i) => {
            let (m, e) = rest.split_at(i);
            let e = &e[1..]; // drop the 'e'/'E'
            (m, e.parse::<i64>().ok()?)
        }
        None => (rest, 0),
    };
    // Split the mantissa into integer + fractional digit strings.
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, f),
        None => (mantissa, ""),
    };
    // Validity: both parts (after splitting) must be all-digits, and at least one
    // digit must be present overall. (serde_json already validated the literal is a
    // well-formed JSON number, but we re-check to stay self-contained and reject any
    // surprise.)
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    // The value is `(int_part . frac_part) * 10^exp`. Its fractional-digit count
    // BEFORE applying the exponent is `frac_part.len()`. The net base-10 exponent
    // applied to the integer coefficient (int_part ++ frac_part, leading zeros
    // irrelevant) is `exp - frac_part.len()`. The number is INTEGRAL iff that net
    // exponent is `>= 0` after trimming trailing zeros from the coefficient — but we
    // only need integrality, which holds iff every fractional digit is cancelled by
    // a non-negative net exponent. Equivalently: integral iff `exp >= frac_len`, OR
    // the fractional tail beyond `frac_len - exp` digits is all zeros.
    let frac_len = frac_part.len() as i64;
    let net_exp = exp - frac_len; // exponent on the all-digit coefficient
                                  // Build the exact integer coefficient from int_part ++ frac_part.
    let mut coeff_digits = String::with_capacity(int_part.len() + frac_part.len());
    coeff_digits.push_str(int_part);
    coeff_digits.push_str(frac_part);
    // Parse coefficient as an exact big integer to avoid any precision loss.
    use num_traits::{Signed, ToPrimitive, Zero};
    let coeff = num_bigint::BigInt::parse_bytes(coeff_digits.as_bytes(), 10)?;
    // ZERO short-circuit (mirrors `Data.Scientific.normalize`, the smart
    // constructor every parsed `Scientific` flows through):
    //
    //   normalize (Scientific c e)
    //     | c > 0     = normalizePositive   c  e
    //     | c < 0     = -(normalizePositive (-c) e)
    //     | otherwise {- c == 0 -} = Scientific 0 0
    //
    // A zero coefficient forces the exponent to 0, so `toBoundedInteger`
    // computes `toIntegral 0 0 = 0 * magnitude 0 = 0` and NEVER evaluates
    // `10 ^ e`. We must do the same: with `coeff == 0` the value is `0`
    // regardless of `net_exp`, and `0` is in the Word8 range `[0, 255]`.
    // Returning early avoids `BigInt::from(10).pow(net_exp as u32)` blowing up
    // (or the `i64 -> u32` truncation) for literals like `0e2000000000`.
    if coeff.is_zero() {
        return Some(0);
    }
    // `dangerouslyBig` guard (Data.Scientific.toBoundedInteger), specialised to
    // `Word8` and avoiding any `10^net_exp` allocation for an out-of-range or
    // non-integral literal. In `Data.Scientific`:
    //
    //   toBoundedInteger s
    //     | c == 0    = fromIntegerBounded 0
    //     | integral  = if dangerouslyBig then Nothing else fromIntegerBounded n
    //     | otherwise = Nothing
    //     where
    //       c = coefficient s
    //       e = base10Exponent s                     -- RAW, unnormalized exponent
    //       integral = e >= 0 || e' >= 0
    //       dangerouslyBig = e > limit &&
    //                        e > integerLog10' (max (abs iMinBound) (abs iMaxBound))
    //       n = toIntegral s'                         -- LAZY: 10^e magnitude
    //
    // and the upstream comment on `n` reads: "This should not be evaluated if the
    // given Scientific is dangerouslyBig since it could consume all space and crash
    // the process." For `Word8`, `iMinBound = 0`, `iMaxBound = 255`, so
    // `integerLog10' (max (abs 0) (abs 255)) = integerLog10' 255 = 2`.
    //
    // The Aeson/Scientific parser builds the coefficient from the concatenated
    // int++frac digit string (UNNORMALIZED) and sets `base10Exponent = explicitExp -
    // numFracDigits`, which is exactly our `net_exp` on the all-digit `coeff` (>= 1
    // after the `c == 0` short-circuit). Since `coeff >= 1`:
    //
    //   * net_exp >= 0 branch: `coeff * 10^net_exp >= 10^net_exp`, so `net_exp >= 3`
    //     forces a value `>= 1000 > 255` and is rejected WITHOUT the pow (10^3 = 1000
    //     already exceeds u8::MAX, hence the `>= 3` bound). For `net_exp in {0,1,2}`
    //     the magnitude `10^net_exp <= 100` is trivially bounded, so we compute it.
    //
    //   * net_exp < 0 branch: the value is integral iff `coeff` is divisible by
    //     `10^|net_exp|`, i.e. iff `coeff` has at least `|net_exp|` trailing zero
    //     DIGITS. We count trailing zero digits directly (no `10^|net_exp|`), so
    //     `1e-2000000000` and `1.5` are rejected in O(1). When `|net_exp|` does not
    //     exceed the trailing-zero count it is at most the digit-length of `coeff`,
    //     so the divisor `10^|net_exp|` is bounded by `coeff` itself and is safe.
    let value: num_bigint::BigInt = if net_exp >= 0 {
        // `coeff >= 1`, so any `net_exp >= 3` exceeds u8::MAX (10^3 = 1000 > 255)
        // regardless of the exact coefficient: reject WITHOUT computing the pow.
        // This is the byte-exact `dangerouslyBig` short-circuit for Word8 — e.g.
        // `1e9` and `1e2000000000` return `None` in O(1) with no allocation.
        if net_exp >= 3 {
            return None;
        }
        // net_exp in {0, 1, 2}: 10^net_exp <= 100, bounded.
        let pow = num_bigint::BigInt::from(10u8).pow(net_exp as u32);
        coeff * pow
    } else {
        // Integral iff `coeff` divisible by 10^|net_exp|, which requires `coeff` to
        // have >= |net_exp| trailing zero digits. Count them WITHOUT the pow so an
        // enormous |net_exp| (e.g. `1e-2000000000`) returns `None` in O(1).
        let needed = net_exp.unsigned_abs();
        if trailing_zero_digits(&coeff) < needed {
            return None; // non-integral (e.g. 1.5, 1.0000000000000001, 1e-2000000000)
        }
        // `needed` <= trailing-zero count <= digit length of `coeff`, so the divisor
        // 10^needed is bounded by `coeff`'s magnitude and is safe to materialise.
        let pow = num_bigint::BigInt::from(10u8).pow(needed as u32);
        let (q, r) = num_integer_div_rem(&coeff, &pow);
        debug_assert!(
            r.is_zero(),
            "trailing-zero-digit count guarantees divisibility"
        );
        q
    };
    let value = if negative { -value } else { value };
    // Word8 range check [0, 255].
    if value.is_negative() {
        return None;
    }
    let n = value.to_u64()?;
    if n <= u8::MAX as u64 {
        Some(n)
    } else {
        None
    }
}

/// Count the trailing zero DIGITS of a non-zero `BigInt` magnitude (base 10).
///
/// `coeff` is `coefficient * 10^k` iff its decimal representation ends in at least
/// `k` zero digits, so this is the integrality test for `net_exp < 0` WITHOUT
/// materialising `10^|net_exp|` (which would blow up for a literal like
/// `1e-2000000000`). The work is bounded by the decimal length of `coeff`, which is
/// itself bounded by the source literal length. `coeff` is always non-zero here
/// (the `c == 0` short-circuit ran earlier), so the all-zeros case never arises.
fn trailing_zero_digits(coeff: &num_bigint::BigInt) -> u64 {
    // `coeff` is parsed from a digit string and is therefore non-negative, so the
    // base-10 string has no sign prefix; counting trailing '0' bytes is exact.
    let digits = coeff.to_str_radix(10);
    digits.bytes().rev().take_while(|&b| b == b'0').count() as u64
}

/// `BigInt` truncating div/rem (toward zero); both operands are non-negative here.
fn num_integer_div_rem(
    a: &num_bigint::BigInt,
    b: &num_bigint::BigInt,
) -> (num_bigint::BigInt, num_bigint::BigInt) {
    (a / b, a % b)
}

/// The only snapshot backend dugite's InMemory MemPack importer can read.
///
/// Upstream tags an InMemory (V2) snapshot's `meta` with `backend == "utxohd-mem"`
/// (`UTxOHDMemSnapshot`). The V2/InMemory `loadSnapshot` enforces this
/// unconditionally before decoding the tables:
///
/// ```haskell
/// -- Ouroboros.Consensus.Storage.LedgerDB.V2.InMemory
/// when (snapshotBackend /= UTxOHDMemSnapshot) $
///   throwE $ MetadataBackendMismatch snapshotBackend
/// ```
pub const UTXO_HD_MEM_BACKEND_TAG: &str = "utxohd-mem";

/// Parse and ENFORCE the `backend` field of a snapshot `meta` JSON blob.
///
/// STRICT (#10, re-gauntlet w4007sv2k). The V2/InMemory `loadSnapshot`
/// validates `when (snapshotBackend /= UTxOHDMemSnapshot) $ throwE
/// MetadataBackendMismatch` BEFORE decoding the tables (which it then decodes
/// via the unconditional `BigEndianTxIx` MemPack instance — it never branches on
/// the codec version for endianness). We mirror that: a present meta whose
/// `backend` is absent, null, non-string, or anything other than
/// [`UTXO_HD_MEM_BACKEND_TAG`] is a HARD ERROR.
///
/// Returns `Ok(())` only for `backend == "utxohd-mem"`.
pub fn enforce_snapshot_backend_is_utxohd_mem(meta_json: &[u8]) -> Result<(), SerializationError> {
    let value: serde_json::Value = serde_json::from_slice(meta_json).map_err(|e| {
        SerializationError::CborDecode(format!("snapshot meta is not valid JSON: {e}"))
    })?;
    match value.get("backend").and_then(|b| b.as_str()) {
        Some(UTXO_HD_MEM_BACKEND_TAG) => Ok(()),
        other => Err(SerializationError::CborDecode(format!(
            "snapshot meta backend={other:?} is not `{UTXO_HD_MEM_BACKEND_TAG}` \
             (UTxOHDMemSnapshot); upstream V2/InMemory loadSnapshot rejects this with \
             MetadataBackendMismatch before decoding tables — refusing to import"
        ))),
    }
}

/// Number of leading entries [`detect_txix_endianness`] samples before deciding.
const TXIX_DETECT_SAMPLE: usize = 2000;

/// A decoded MemPack TxIn.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MemPackTxIn {
    /// Transaction hash (32 bytes, big-endian).
    pub txid: Hash<32>,
    /// Output index. Decoded from the 2-byte on-disk trailer using the snapshot's
    /// empirically-determined [`TxIxEndianness`].
    pub txix: u16,
}

/// Decode a MemPack TxIn from exactly 34 raw bytes.
///
/// Layout: `TxId(32 BE) || TxIx(2)`. The `TxIx` byte order is snapshot-dependent
/// and supplied by the caller via `endianness` — it is NOT derivable from the
/// filesystem layout (the flat-tables move and the big-endian flip landed in
/// different `ouroboros-consensus` releases) nor from the blob content (flat-LE
/// and flat-BE blobs are byte-identical). The importer determines it empirically
/// via [`detect_txix_endianness`]. See [`TxIxEndianness`] and the module docs.
pub fn decode_mempack_txin(
    data: &[u8],
    endianness: TxIxEndianness,
) -> Result<MemPackTxIn, SerializationError> {
    if data.len() != 34 {
        return Err(SerializationError::InvalidLength {
            expected: 34,
            got: data.len(),
        });
    }
    let mut txid_bytes = [0u8; 32];
    txid_bytes.copy_from_slice(&data[0..32]);
    let ix_bytes = [data[32], data[33]];
    let txix = match endianness {
        // LEGACY: raw host-native `MemPack Word16`.
        // Canonical: Cardano.Ledger.TxIn `instance MemPack TxIn` over
        // `newtype TxIx = TxIx Word16 deriving newtype (… MemPack)`.
        TxIxEndianness::Little => u16::from_le_bytes(ix_bytes),
        // NEW: `BigEndianTxIn` byte-swaps the index.
        // Canonical: Ouroboros.Consensus.Shelley.Ledger.Ledger
        // `packM (BigEndianTxIx (TxIx w)) = packM (byteSwap16 w)`.
        TxIxEndianness::Big => u16::from_be_bytes(ix_bytes),
    };
    Ok(MemPackTxIn {
        txid: Hash::from_bytes(txid_bytes),
        txix,
    })
}

/// Histogram of how "sane" a set of decoded `TxIx` values looks.
///
/// Real UTxO indices cluster in `0..~20`; a WRONG endianness re-maps true indices
/// `1..=255` onto `{256, 512, …}` (nonzero multiples of 256) and empties out
/// `[1, 255]`. We therefore count entries landing in those two buckets. (Index 0
/// is endianness-invariant — `00 00` decodes to 0 either way — so it is ignored.)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TxIxDistribution {
    /// Entries whose decoded `txix` is in `[1, 255]` (the dense, sane region).
    pub low: u64,
    /// Entries whose decoded `txix` is a nonzero multiple of 256 (the tell-tale
    /// signature of a mis-keyed `[1, 255]` index under the wrong endianness).
    pub mult256: u64,
    /// Total non-zero entries sampled (for context in error messages).
    pub nonzero: u64,
}

impl TxIxDistribution {
    /// Fold one decoded `txix` into the histogram. Index 0 is ignored (it is
    /// endianness-invariant). Used by the importer's hard safety net to validate
    /// the full set after decoding.
    pub fn observe_txix(&mut self, txix: u16) {
        if txix == 0 {
            return;
        }
        self.nonzero += 1;
        if (1..=255).contains(&txix) {
            self.low += 1;
        }
        if txix.is_multiple_of(256) {
            self.mult256 += 1;
        }
    }

    /// A distribution is "sane" when the dense `[1, 255]` region dominates the
    /// nonzero-multiple-of-256 region (the mis-key signature). When there are no
    /// nonzero indices at all the sample is uninformative and treated as sane.
    ///
    /// Threshold: `low > mult256 * 8`. Real snapshots have `low` in the thousands
    /// and `mult256` in the low single digits (a handful of genuine 256/512
    /// indices on very large txs), so the margin is enormous; a mis-keyed
    /// distribution inverts the relationship entirely.
    pub fn is_sane(&self) -> bool {
        self.nonzero == 0 || self.low > self.mult256.saturating_mul(8)
    }
}

/// Sample the first [`TXIX_DETECT_SAMPLE`] entries of a `tables`/`tvar` blob and
/// decode each `TxIx` under BOTH endiannesses, returning the one whose index
/// distribution is sane.
///
/// IMPORTANT: this is **NOT** the decision-maker. The authoritative byte order
/// comes from the snapshot codec version (see
/// [`TxIxEndianness::from_tables_codec_version`]). This empirical detector is used
/// only as an INDEPENDENT cross-validation of that decision (see
/// [`cross_validate_txix_endianness`]) — it reads the actual data, so it catches a
/// version that disagrees with reality. It also remains a convenience for tests /
/// fixtures that ship no `meta` file.
///
/// Decision rule (see [`TxIxDistribution::is_sane`]): real UTxO indices are
/// heavily concentrated in `[1, 255]`; the wrong endianness pushes those onto
/// nonzero multiples of 256. We compute both distributions and:
/// * if exactly one is sane, choose it;
/// * if both are sane (e.g. an all-index-0 snapshot — endianness-invariant, so
///   either is correct), prefer [`TxIxEndianness::Big`] (the modern default);
/// * if neither is sane, return an error — refuse to guess and risk mis-keying.
pub fn detect_txix_endianness(data: &[u8]) -> Result<TxIxEndianness, SerializationError> {
    let mut le = TxIxDistribution::default();
    let mut be = TxIxDistribution::default();

    // Iterate raw keys directly (do NOT go through TvarIterator, which itself
    // needs an endianness). We re-walk the CBOR map skeleton and sample the
    // 34-byte keys, decoding the 2-byte TxIx trailer both ways.
    let mut walker = RawKeyWalker::new(data)?;
    let mut sampled = 0usize;
    while sampled < TXIX_DETECT_SAMPLE {
        let Some(key) = walker.next_key()? else { break };
        let ix_bytes = [key[32], key[33]];
        le.observe_txix(u16::from_le_bytes(ix_bytes));
        be.observe_txix(u16::from_be_bytes(ix_bytes));
        sampled += 1;
    }

    match (le.is_sane(), be.is_sane()) {
        (true, false) => Ok(TxIxEndianness::Little),
        (false, true) => Ok(TxIxEndianness::Big),
        // Both sane (e.g. all-zero indices): the snapshot is endianness-invariant
        // for the sampled entries; default to the modern BE format.
        (true, true) => Ok(TxIxEndianness::Big),
        (false, false) => Err(SerializationError::CborDecode(format!(
            "TxIx endianness detection failed: neither byte order yields a sane \
             index distribution (LE: low={} mult256={} nonzero={}; BE: low={} \
             mult256={} nonzero={}). Refusing to import a possibly mis-keyed UTxO set.",
            le.low, le.mult256, le.nonzero, be.low, be.mult256, be.nonzero
        ))),
    }
}

/// INDEPENDENT cross-validation of an authoritatively-chosen [`TxIxEndianness`].
///
/// `chosen` is the byte order derived from the snapshot codec version (the
/// decision-maker, [`TxIxEndianness::from_tables_codec_version`]). This function
/// re-derives the byte order empirically from the *data* ([`detect_txix_endianness`])
/// and ERRORs LOUD if the data DEFINITELY contradicts the version — defense in
/// depth against a snapshot whose `meta` is wrong/forged or a future format change.
///
/// It is deliberately a no-op when the data is endianness-invariant or genuinely
/// ambiguous (e.g. an all-index-0 sample, where both byte orders look sane): in
/// that case the empirical signal cannot refute the authoritative version, so we
/// trust the version (the cardinal rule — the version decides, the heuristic only
/// vetoes a clear contradiction).
pub fn cross_validate_txix_endianness(
    data: &[u8],
    chosen: TxIxEndianness,
) -> Result<(), SerializationError> {
    let mut le = TxIxDistribution::default();
    let mut be = TxIxDistribution::default();
    let mut walker = RawKeyWalker::new(data)?;
    let mut sampled = 0usize;
    while sampled < TXIX_DETECT_SAMPLE {
        let Some(key) = walker.next_key()? else { break };
        let ix_bytes = [key[32], key[33]];
        le.observe_txix(u16::from_le_bytes(ix_bytes));
        be.observe_txix(u16::from_be_bytes(ix_bytes));
        sampled += 1;
    }

    // The empirical signal can only REFUTE the version when exactly one byte order
    // is sane AND it is the opposite of `chosen`. If both are sane (invariant /
    // ambiguous) or both are insane (uninformative here), we defer to the version.
    let empirical = match (le.is_sane(), be.is_sane()) {
        (true, false) => Some(TxIxEndianness::Little),
        (false, true) => Some(TxIxEndianness::Big),
        _ => None,
    };
    match empirical {
        Some(emp) if emp != chosen => Err(SerializationError::CborDecode(format!(
            "TxIx endianness cross-validation FAILED: snapshot codec version selected \
             {chosen:?} but the index distribution unambiguously indicates {emp:?} \
             (LE: low={} mult256={} nonzero={}; BE: low={} mult256={} nonzero={}). \
             The snapshot meta disagrees with the data; refusing to import.",
            le.low, le.mult256, le.nonzero, be.low, be.mult256, be.nonzero
        ))),
        _ => Ok(()),
    }
}

/// HARD SAFETY NET: assert that a decoded `TxIx` distribution is sane, returning
/// a loud error otherwise so the importer can REFUSE the import rather than
/// silently store mis-keyed UTxO keys (the cardinal rule: no silent corruption).
///
/// Call this after decoding the full set (or a representative prefix) with the
/// chosen endianness. It is the last line of defence regardless of how the
/// endianness was selected (authoritative codec version, empirical detection, or
/// an explicit override).
pub fn assert_txix_distribution_sane(
    dist: &TxIxDistribution,
    endianness: TxIxEndianness,
) -> Result<(), SerializationError> {
    if dist.is_sane() {
        Ok(())
    } else {
        Err(SerializationError::CborDecode(format!(
            "imported UTxO TxIx distribution looks mis-keyed under {endianness:?}: \
             low([1,255])={} must dominate mult256={} (nonzero={}). This is the \
             signature of a wrong-endianness import; refusing to store corrupt keys.",
            dist.low, dist.mult256, dist.nonzero
        )))
    }
}

/// Walks the `array(1) [ map { bytes(34) => bytes(N), … } ]` skeleton, yielding
/// each 34-byte TxIn key WITHOUT decoding it (used by endianness detection, which
/// must inspect raw key bytes before an endianness is known).
struct RawKeyWalker<'a> {
    data: &'a [u8],
    offset: usize,
    finished: bool,
}

impl<'a> RawKeyWalker<'a> {
    fn new(data: &'a [u8]) -> Result<Self, SerializationError> {
        // RawKeyWalker is used only for endianness DETECTION (sampling raw 34-byte
        // keys), not for the authoritative import, so it does not need the map-kind /
        // missing-break truncation check; it stops cleanly at exhaustion either way.
        let offset = tvar_body_offset(data)?.offset;
        Ok(RawKeyWalker {
            data,
            offset,
            finished: false,
        })
    }

    /// Return the next 34-byte key, or `None` at the map break / truncation.
    fn next_key(&mut self) -> Result<Option<&'a [u8]>, SerializationError> {
        if self.finished {
            return Ok(None);
        }
        let remaining = &self.data[self.offset..];
        if remaining.is_empty() || remaining[0] == 0xff {
            self.finished = true;
            return Ok(None);
        }
        let (key_bytes, key_consumed) = match decode_bytes(remaining) {
            Ok(v) => v,
            Err(_) => {
                self.finished = true;
                return Ok(None);
            }
        };
        if key_bytes.len() != 34 {
            self.finished = true;
            return Err(SerializationError::InvalidLength {
                expected: 34,
                got: key_bytes.len(),
            });
        }
        // Skip the value bytes to position at the next key.
        let val_start = self.offset + key_consumed;
        if val_start >= self.data.len() {
            self.finished = true;
            return Ok(Some(key_bytes));
        }
        match decode_bytes(&self.data[val_start..]) {
            Ok((_val, val_consumed)) => {
                self.offset = val_start + val_consumed;
            }
            Err(_) => {
                self.finished = true;
            }
        }
        Ok(Some(key_bytes))
    }
}

/// Result of parsing the `array(1)` + `map` headers of a `tvar` blob: the byte
/// offset of the first map entry, plus whether the map header was an INDEFINITE-length
/// map (CBOR `0xbf`, major 5 / additional-info 31) or a DEFINITE-length map (`0xa0`…).
///
/// The map kind is load-bearing for truncation detection (#10 round-5 R3): an
/// indefinite map MUST terminate with a `0xff` break byte, so EOF reached at an entry
/// boundary WITHOUT having seen that break is a partial-EOF that Haskell `loadSnapshot`
/// rejects. A definite map carries no break byte and legitimately ends at exhaustion.
struct TvarBody {
    offset: usize,
    /// `true` when the map header was the indefinite-length form (`0xbf`); a missing
    /// `0xff` break is then a truncation. `false` for a definite-length map.
    indefinite: bool,
}

/// Parse the `array(1)` + `map` headers and return the byte offset of the first
/// map entry together with the map kind. Shared by [`RawKeyWalker`] and
/// [`TvarIterator`].
fn tvar_body_offset(data: &[u8]) -> Result<TvarBody, SerializationError> {
    if data.is_empty() {
        return Err(SerializationError::CborDecode("empty tvar input".into()));
    }

    let mut off = 0;

    // Decode array(1) header.
    let (arr_len, n) = decode_array_len(data)?;
    if arr_len != 1 {
        return Err(SerializationError::CborDecode(format!(
            "tvar: expected array(1), got array({arr_len})"
        )));
    }
    off += n;

    // Expect indefinite-length map (0xbf) or definite-length map.
    if off >= data.len() {
        return Err(SerializationError::CborDecode(
            "tvar: truncated before map header".into(),
        ));
    }
    let major = data[off] >> 5;
    if major != 5 {
        return Err(SerializationError::CborDecode(format!(
            "tvar: expected map (major 5), got major {major}"
        )));
    }
    let info = data[off] & 0x1f;
    let indefinite;
    if info == 31 {
        // Indefinite map (0xbf): MUST be 0xff-terminated.
        indefinite = true;
        off += 1;
    } else {
        // Definite-length map: skip the header (we iterate until done). No break byte.
        indefinite = false;
        let hdr_size = match info {
            0..=23 => 1,
            24 => 2,
            25 => 3,
            26 => 5,
            27 => 9,
            _ => {
                return Err(SerializationError::CborDecode(
                    "tvar: invalid map length encoding".into(),
                ))
            }
        };
        off += hdr_size;
    }
    Ok(TvarBody {
        offset: off,
        indefinite,
    })
}

/// Iterator over entries in a `tvar` file.
///
/// Yields `(MemPackTxIn, MemPackTxOut)` pairs. Normal termination is EITHER the
/// indefinite-map break byte (`0xff`, for a `0xbf …` map) OR exhaustion exactly at an
/// entry boundary of a DEFINITE-length map (`0xa0 …`), both of which yield `None`.
///
/// A non-empty remainder that cannot be decoded as a complete entry (truncated/
/// malformed key, missing value, or truncated value) is a HARD ERROR: the iterator
/// yields `Some(Err(..))` rather than silently finishing. So too is an INDEFINITE-map
/// blob that reaches EOF at an entry boundary WITHOUT its `0xff` break byte (#10
/// round-5 R3): that is a truncated prefix, and returning `None` would silently import
/// it as a complete UTxO set. This mirrors Haskell `loadSnapshot`, where
/// `readIncremental … valuesMKDecoder` surfaces any CBOR `Fail`/partial-EOF/leftover-
/// bytes condition (including an unterminated indefinite-length item) as
/// `InitFailureRead . ReadSnapshotFailed` and aborts the whole import
/// (ouroboros-consensus `…/LedgerDB/V2/InMemory.hs`) — never a partial UTxO set.
pub struct TvarIterator<'a> {
    data: &'a [u8],
    offset: usize,
    finished: bool,
    endianness: TxIxEndianness,
    /// `true` when the tables blob's map header was the CBOR indefinite-length form
    /// (`0xbf`), which MUST terminate with a `0xff` break byte. When this is set and
    /// the stream runs out at an entry boundary WITHOUT a `0xff` having been seen, the
    /// blob is TRUNCATED and the iterator yields `Some(Err(..))` rather than `None`
    /// (#10 round-5 R3). A definite-length map (`false`) carries no break byte and ends
    /// cleanly at exhaustion of its declared entries.
    map_indefinite: bool,
    /// Set once the `0xff` break byte of an indefinite map has been consumed, so the
    /// EOF-at-boundary branch can distinguish a clean (break-terminated) end from a
    /// truncated prefix.
    saw_break: bool,
}

impl<'a> TvarIterator<'a> {
    /// Create a new iterator, **auto-detecting** the `TxIx` endianness from the
    /// blob's index distribution (see [`detect_txix_endianness`]).
    ///
    /// This is the robust default: it works for any layout/codec version because
    /// it reads the actual data rather than guessing from the filesystem layout
    /// (which is NOT a valid proxy — see the module docs). Use
    /// [`TvarIterator::new_with_endianness`] when the endianness is already known
    /// (an authoritative codec-version signal, or a test fixture).
    pub fn new(data: &'a [u8]) -> Result<Self, SerializationError> {
        let endianness = detect_txix_endianness(data)?;
        Self::new_with_endianness(data, endianness)
    }

    /// Create a new iterator positioned after the `array(1)` and map headers,
    /// decoding each `TxIx` with the given [`TxIxEndianness`].
    pub fn new_with_endianness(
        data: &'a [u8],
        endianness: TxIxEndianness,
    ) -> Result<Self, SerializationError> {
        let TvarBody { offset, indefinite } = tvar_body_offset(data)?;
        Ok(TvarIterator {
            data,
            offset,
            finished: false,
            endianness,
            map_indefinite: indefinite,
            saw_break: false,
        })
    }

    /// Return the current byte offset into the underlying data.
    pub fn offset(&self) -> usize {
        self.offset
    }

    /// The [`TxIxEndianness`] this iterator is decoding `TxIx` keys with.
    pub fn endianness(&self) -> TxIxEndianness {
        self.endianness
    }
}

impl<'a> Iterator for TvarIterator<'a> {
    type Item = Result<(MemPackTxIn, txout::MemPackTxOut), SerializationError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }

        let remaining = &self.data[self.offset..];

        // Check for end-of-data (truncated file) or break byte (0xff).
        if remaining.is_empty() {
            self.finished = true;
            // #10 round-5 R3 — an INDEFINITE-length map (`0xbf …`) MUST be terminated
            // by a `0xff` break byte (RFC 8949 §3.2.1: "a break stop code … MUST
            // terminate" an indefinite-length item). A blob that ends EXACTLY at an
            // entry boundary — a complete (key,value) pair, but NO trailing `0xff` — is
            // a TRUNCATED prefix, not a clean end. Returning `None` here would silently
            // import that prefix as a complete UTxO set (the distribution-sanity check
            // passes, since a prefix shares the same key-distribution shape).
            //
            // Haskell `loadSnapshot` decodes the tables with `readIncremental …
            // valuesMKDecoder`; a CBOR decoder driven to partial-EOF before the
            // indefinite-map break surfaces a `Fail`, which ouroboros-consensus wraps as
            // `InitFailureRead . ReadSnapshotFailed` and ABORTS the whole import
            // (`…/LedgerDB/V2/InMemory.hs`) — it never imports a partial map. Mirror
            // that: for an indefinite map whose `0xff` break we have NOT yet seen,
            // HARD-ERROR instead of a silent `None`.
            if self.map_indefinite && !self.saw_break {
                return Some(Err(SerializationError::CborDecode(
                    "tvar: indefinite-length tables map reached EOF at an entry boundary \
                     without its `0xff` break byte — TRUNCATED snapshot prefix. Haskell \
                     loadSnapshot aborts on partial-EOF (ReadSnapshotFailed); refusing a \
                     silent partial UTxO-set import"
                        .into(),
                )));
            }
            // A definite-length map (or an indefinite map already break-terminated) ends
            // cleanly at exhaustion.
            return None;
        }
        if remaining[0] == 0xff {
            // Indefinite-map break byte consumed: this is the LEGITIMATE clean end of an
            // indefinite map. Record it so the EOF branch above (on the next call, were
            // it reached) treats the stream as properly terminated.
            self.finished = true;
            self.saw_break = true;
            return None;
        }

        // Decode CBOR bytes(34) key.
        let key_result = decode_bytes(remaining);
        let (key_bytes, key_consumed) = match key_result {
            Ok(v) => v,
            Err(_) => {
                // A non-empty remainder that does not start with the indefinite
                // break (`0xff`) but cannot be decoded as a complete CBOR bytes
                // header is a TRUNCATED / malformed entry — NOT a clean stream
                // end. Haskell `loadSnapshot` aborts the whole import here:
                // `readIncremental … valuesMKDecoder` surfaces a CBOR `Fail` /
                // partial-EOF / leftover-bytes condition as
                // `InitFailureRead . ReadSnapshotFailed`, throwing rather than
                // silently importing a partial UTxO set (InMemory.hs). Mirror
                // that: HARD-ERROR instead of `finished = true; None`.
                self.finished = true;
                return Some(Err(SerializationError::CborDecode(
                    "tvar: truncated/malformed entry key — refusing a silent partial \
                     import (Haskell loadSnapshot aborts on CBOR Fail/partial-EOF)"
                        .into(),
                )));
            }
        };

        // Decode TxIn from the 34-byte key using the iterator's endianness.
        let txin = match decode_mempack_txin(key_bytes, self.endianness) {
            Ok(t) => t,
            Err(e) => {
                self.finished = true;
                return Some(Err(e));
            }
        };

        // Decode CBOR bytes(N) value.
        let val_start = self.offset + key_consumed;
        if val_start >= self.data.len() {
            // We decoded a key but there are no bytes left for its value: the
            // entry is truncated mid-pair. Haskell `loadSnapshot` aborts on a
            // partial-EOF (`ReadSnapshotFailed`); never import a key with no
            // value. HARD-ERROR.
            self.finished = true;
            return Some(Err(SerializationError::CborDecode(
                "tvar: truncated entry — key present but value bytes missing \
                 (Haskell loadSnapshot aborts on partial-EOF)"
                    .into(),
            )));
        }
        let val_result = decode_bytes(&self.data[val_start..]);
        let (val_bytes, val_consumed) = match val_result {
            Ok(v) => v,
            Err(_) => {
                // Truncated / malformed value — HARD-ERROR, mirroring Haskell
                // `readIncremental … valuesMKDecoder` surfacing a CBOR Fail /
                // partial-EOF as `ReadSnapshotFailed` (InMemory.hs). Never a
                // silent partial import.
                self.finished = true;
                return Some(Err(SerializationError::CborDecode(
                    "tvar: truncated/malformed entry value — refusing a silent \
                     partial import (Haskell loadSnapshot aborts on CBOR Fail)"
                        .into(),
                )));
            }
        };

        // Advance past key + value.
        self.offset = val_start + val_consumed;

        // Decode the MemPack TxOut from the value bytes — FULL-CONSUMPTION-STRICT.
        //
        // `loadSnapshot`'s `valuesMKDecoder` uses `Data.MemPack` top-level
        // `unpackFail`/`unpack`, NOT the leftover-tolerant `unpackLeftOver`:
        //
        //   unpackFail :: forall a b. (MemPack a, Buffer b) => b -> Fail a
        //   unpackFail b = do
        //     let len = bufferByteCount b
        //     (a, consumedBytes) <- unpackLeftOver b
        //     when (consumedBytes /= len) $
        //       unpackFailNotFullyConsumed (typeName @a) consumedBytes len
        //     pure a
        //
        // i.e. any trailing byte inside a value blob (`consumedBytes /= len`)
        // raises `NotFullyConsumed`, which ouroboros-consensus surfaces as
        // `InitFailureRead . ReadSnapshotFailed` (InMemory.hs) and ABORTS the whole
        // snapshot import. The per-tag `decode_tag0..5` decoders return a `consumed`
        // length that can be `< val_bytes.len()` (they stop after the fields they
        // understand), so we MUST assert it consumed the ENTIRE value blob — a
        // discarded `_consumed` would silently import a corrupt/extended TxOut that
        // Haskell rejects. (The KEY side is already full-consumption-strict:
        // `decode_mempack_txin` errors unless `data.len() == 34`.)
        match txout::decode_mempack_txout(val_bytes) {
            Ok((txout, consumed)) => {
                if consumed != val_bytes.len() {
                    self.finished = true;
                    return Some(Err(SerializationError::CborDecode(format!(
                        "tvar: MemPack TxOut value not fully consumed (consumed {} of \
                         {} bytes) — Haskell Data.MemPack unpackFail raises \
                         NotFullyConsumed and loadSnapshot aborts the import \
                         (ReadSnapshotFailed)",
                        consumed,
                        val_bytes.len()
                    ))));
                }
                Some(Ok((txin, txout)))
            }
            Err(e) => {
                self.finished = true;
                Some(Err(e))
            }
        }
    }
}
