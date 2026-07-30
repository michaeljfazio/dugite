---
name: outputtoobigutxo-maxvalsize-exact-mechanics
description: OutputTooBigUTxO/maxValSize exact measurement mechanics (Alonzo.validateOutputTooBigUTxO, live-verified 2026-07-31) — re-encode not wire bytes, Map encodeMap definite/indefinite threshold at 23 entries, contrasted with Sized's wire-byte-span used by the min-UTxO check
metadata:
  type: reference
---

Live-verified against IntersectMBO/cardano-ledger (master) 2026-07-31.

## Call chain (Conway reuses Babbage/Alonzo unchanged)

`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs` `conwayUtxoTransition`
-> `Babbage.babbageUtxoValidation` (`eras/babbage/impl/.../Rules/Utxo.hs`, line ~389)
-> `Alonzo.validateOutputTooBigUTxO pp allOutputs` (`eras/alonzo/impl/.../Rules/Utxo.hs`, line ~412-431)

Conway has NO override of this check — 100% Babbage/Alonzo code runs verbatim.

## The exact check (quoted verbatim)

```haskell
validateOutputTooBigUTxO ::
  ( EraTxOut era, AlonzoEraPParams era, Foldable f ) =>
  PParams era -> f (TxOut era) -> Test (AlonzoUtxoPredFailure era)
validateOutputTooBigUTxO pp outputs =
  failureOnNonEmpty outputsTooBig OutputTooBigUTxO
  where
    maxValSize = pp ^. ppMaxValSizeL
    protVer = pp ^. ppProtocolVersionL
    outputsTooBig = F.foldl' accum [] outputs
    accum ans txOut =
      let v = txOut ^. valueTxOutL
          serSize = fromIntegral $ BSL.length $ serialize (pvMajor protVer) v
       in if serSize > maxValSize
            then (fromIntegral serSize, fromIntegral maxValSize, txOut) : ans
            else ans
```

- Comparison is STRICT (`serSize > maxValSize`): exactly `maxValSize` (5000) is legal.
- `v = txOut ^. valueTxOutL` is the DECODED `Value era` (= `MaryValue` from Mary onward, unchanged through Conway) — NOT raw wire bytes.
- `serialize :: EncCBOR a => Version -> a -> BSL.ByteString; serialize version = toLazyByteStringWith strategy mempty . toBuilder version . encCBOR` (`libs/cardano-ledger-binary/.../Encoding.hs`) — this is a FRESH RE-ENCODE of the decoded structured value via `encCBOR`, called at the pinned `Version = pvMajor protVer`. `ProtVer{pvMajor :: !Version, pvMinor :: !Word32}` (`libs/cardano-ledger-core/.../BaseTypes.hs`) — `Version` (`libs/cardano-ledger-binary/.../Version.hs`, `newtype Version = Word32`) numbers ARE the protocol major version numbers 1:1 (`byronProtVer = natVersion @1`, `shelleyProtVer = natVersion @2`, Conway PV9/PV10, etc.) — no separate mapping/conversion.
- CRITICAL: `MaryValue` in a `TxOut` is a plain decoded value, NOT `MemoBytes`-backed (unlike Timelock/native scripts, see [[native-script-hash-memobytes-safetohash]]) — there is no raw-byte preservation for it, so the check ALWAYS measures the canonical re-encode, never the original wire bytes. A non-canonical wire encoding (indefinite map where canonical would be definite, or vice versa) is NORMALIZED AWAY by this re-encode — Haskell's measured size can differ from the tx's actual wire-byte length for that Value.

## MaryValue/MultiAsset encoding (eras/mary/impl/.../Value.hs)

```haskell
instance EncCBOR MaryValue where
  encCBOR (MaryValue c ma@(MultiAsset m)) =
    if Map.null m
      then encCBOR c                      -- ada-only: BARE coin int, no array wrapper
      else encode $ Rec MaryValue !> To c !> To ma   -- multiasset: definite-length array [coin, multiasset-map]
```
`Rec` (`Coders.hs`, `Closed Dense`) always emits `encodeListLen n` — DEFINITE length array, never indefinite, for this 2-field product.

`newtype MultiAsset = MultiAsset (Map PolicyID (Map AssetName Integer)) deriving (..., EncCBOR)` — `GeneralizedNewtypeDeriving` (pragma present in file) makes `EncCBOR MultiAsset` identical to `EncCBOR (Map PolicyID (Map AssetName Integer))`.

`instance (EncCBOR k, EncCBOR v) => EncCBOR (Map.Map k v) where encCBOR = encodeMap encCBOR encCBOR` (`EncCBOR.hs`) applies at BOTH map nesting levels (outer PolicyID map AND every inner AssetName map independently).

## encodeMap — THE size-threshold behavior (libs/cardano-ledger-binary/.../Encoder.hs, ~line 391-443)

```haskell
-- | Encode a Map. Versions variance:
-- * [>= 2] - Variable length encoding for Maps larger than 23 key value pairs, otherwise exact length encoding
-- * [< 2]  - always exact/definite length encoding
encodeMap encodeKey encodeValue m =
  let mapEncoding = Map.foldMapWithKey (\k v -> encodeKey k <> encodeValue v) m
   in ifEncodingVersionAtLeast (natVersion @2)
        (variableMapLenEncoding (Map.size m) mapEncoding)   -- used when version >= 2
        (exactMapLenEncoding (Map.size m) mapEncoding)       -- used when version < 2

exactMapLenEncoding len contents = encodeMapLen (fromIntegral len) <> contents   -- always definite header

variableMapLenEncoding len contents =
  if len <= lengthThreshold                                  -- lengthThreshold = 23
    then exactMapLenEncoding len contents                     -- N<=23: DEFINITE header (1 byte for N in 0-23)
    else encodeMapLenIndef <> contents <> encodeBreak          -- N>23: INDEFINITE (0xbf ... 0xff)
```
`ifEncodingVersionAtLeast atLeast newer older` picks `newer` when current pinned Version >= atLeast, else `older` — verified from its own definition (`Encoding $ \cur -> if cur >= atLeast then newerEncoding cur else olderEncoding cur`). Since Conway PV9/PV10 >> 2, EVERY Mary+ value on Conway is in the "variableMapLenEncoding" (threshold) regime; the `< 2` branch is Byron-only dead weight for this type (Value/MultiAsset didn't exist before Mary=PV4 anyway).

Byte-cost analysis (why the threshold is chosen at 23, and where naive reimplementations diverge):
- N in 0-23: definite header = 1 byte (major-type-5 initial byte encodes count inline, 0xa0-0xb7). Indefinite would cost 2 bytes (0xbf open + 0xff break). Haskell ALWAYS picks definite here → 1-byte header.
- N in 24-255: definite header = 2 bytes (0xb8 + 1-byte count). Indefinite = 2 bytes (0xbf+0xff). IDENTICAL total overhead either way — no divergence possible from this switch in this range.
- N > 255: definite header grows to 3/5/9 bytes (0xb9/0xba/0xbb + wider count); indefinite stays fixed at 2 bytes. This is why Haskell switches at N>23 — pure future-proofing, not relevant for realistic maxValSize~5000-byte outputs (N>255 assets in one map would need >255*~30 bytes just for entries, blowing past any real maxValSize long before N reaches 256).

**Practical conclusion: for any realistic multiasset TxOut (few-to-dozens of policies/assets, which is the overwhelming common case), the N<=23 branch is what's live, and Haskell emits a 1-byte DEFINITE header per map level (outer policy map + each inner asset map, independently).** A Rust/other reimplementation that always emits INDEFINITE-length CBOR maps for MultiAsset (e.g., a streaming-only encoder, or one that didn't replicate this exact-vs-variable optimization) will overshoot by exactly +1 byte per map level that has <=23 entries — i.e. it will compute a value 1 (or 2, if BOTH the outer and one inner map are affected) bytes larger than Haskell's canonical `serialize` result. This is the single most likely explanation for a "measured 5001, Haskell accepted at 5000 (maxValSize=5000)" divergence, since almost all real multi-asset outputs have small (<=23) map sizes at every level.

## Coin / quantity encoding

`newtype Coin = Coin {unCoin :: Integer} deriving newtype (..., EncCBOR)` -> delegates straight to `EncCBOR Integer` (`encCBOR = encodeInteger`), which itself is `cborg`'s canonical/minimal-width CBOR integer encoder (major type 0 non-negative / major type 1 negative, minimal argument width per RFC 8949 rules — 0-23 inline, 24-255 1-byte arg, 256-65535 2-byte arg, etc). `MultiAsset` inner-map values are plain `Integer` too (`Map AssetName Integer`), encoded the same way — no special-casing for large/negative quantities beyond what `cborg`'s `encodeInteger` already does. Decode-side bounds quantities to `Int64` range (`decodeIntegerBounded64` in Value.hs) but that's a decode-only guard; encode-side has no bound.

## Contrast: the OTHER "size" check on the same TxOut uses REAL wire bytes, not a re-encode

`validateOutputTooSmallUTxO` (min-UTxO-value, `coinsPerUTxOByte` check) operates on `Sized (TxOut era)`, NOT the plain `TxOut`. `Sized`'s `sizedSize` (`libs/cardano-ledger-binary/.../Decoding/Sized.hs`) is populated by `decodeSized`:
```haskell
decodeSized decoder = do
  Annotated v (ByteSpan start end) <- annotatedDecoder decoder
  pure $! Sized v $! end - start
```
— this IS the actual original wire-byte span of the whole TxOut as it appeared on the wire (`ByteSpan`/`Annotated`, ledger's byte-capture-during-decode machinery, same family as `MemoBytes`/`KeepRaw`). `Alonzo.validateOutputTooBigUTxO`, by contrast, is handed `allOutputs = fmap sizedValue allSizedOutputs` — the `Sized` wrapper (and its wire-byte-span) is EXPLICITLY DISCARDED before this check runs; only the plain decoded `TxOut` reaches it, and it re-derives its own measurement purely via `encCBOR`/`serialize` on just the `Value` field. **These are two different measurement mechanisms sitting a few lines apart in the same function (`Babbage.babbageUtxoValidation`) — min-UTxO-size = real wire bytes of the whole TxOut; maxValSize = canonical re-encode of just the Value.** Do not let one implementation accidentally share code/bytes with the other.

## Rust Translation Notes (Dugite)

- `crates/dugite-ledger` wherever `OutputTooBigUTxO`/maxValSize is checked: must build the size from a canonical re-encode of the decoded `Value`/multiasset structure using pallas' or dugite's own CBOR writer — never from a captured wire-byte span for this specific check (that's fine/required for the SEPARATE min-UTxO check, which is legitimately wire-byte-based).
- The map writer used for MultiAsset (both PolicyID->AssetMap and AssetName->Quantity levels) must replicate `encodeMap`'s threshold: definite-length header when entry count <= 23, indefinite (open+break) when > 23. A writer that always does definite-length (canonical minicbor-style) is actually BYTE-COMPATIBLE with Haskell for N in 0-23 AND N in 24-255 (both cost the same overhead in that range, see analysis above) — only diverges for N>255, which is unreachable under realistic maxValSize budgets. A writer that always does INDEFINITE-length is the dangerous case: it overshoots by +1 byte per map level whenever that level has <=23 entries, which is the common case — prime suspect for a "measured N+1 vs Haskell's N" divergence.
- Ada-only values must serialize as a bare minimal-width integer (no array wrapper) — not `[coin]`.
- Multiasset values must serialize as a definite-length 2-array `[coin, multiasset_map]` — never indefinite at the outer TxOut-value level (that's the `Rec`/product encoding, distinct from the inner Map encoding rule above).
- Coin/quantity minimal-width integer encoding should already be provided by any RFC-8949-compliant CBOR writer (pallas' `minicbor` included) — low risk area.
