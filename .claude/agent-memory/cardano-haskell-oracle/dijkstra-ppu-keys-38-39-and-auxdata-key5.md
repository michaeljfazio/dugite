---
name: dijkstra-ppu-keys-38-39-and-auxdata-key5
description: Verified (gh api, SHA-pinned) wire types for Dijkstra ParameterChange keys 38/39 and AuxData key 5 PlutusV4 status
type: reference
---

Pinned commit verified real via `gh api repos/IntersectMBO/cardano-ledger/commits/<sha>`:
`4849c13d6f70e5ab46add9af6e0ec5c537b61f69` (merge of PR #5950, 2026-08-04,
GPG-signed/verified). The commit's own diff is unrelated (drops
`EncCBORGroup BlockBody`), but the file tree AT that commit is genuine —
confirms dugite issue #1013's citation was not hallucinated.

## Key 38 `maxPledgeLeverage` (CIP-50)

`dppMaxPledgeLeverage :: THKD (...) f MaxPledgeLeverage`
— `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/PParams.hs:167`
`PParam` record with `ppuTag = 38` at same file, lines 546-558.

`MaxPledgeLeverage` newtype — `libs/cardano-ledger-core/src/Cardano/Ledger/Core/PParams.hs:367-377`:
```haskell
newtype MaxPledgeLeverage = MaxPledgeLeverage
  { unMaxPledgeLeverage :: StrictMaybe NonNegativeInterval }
instance EncCBOR MaxPledgeLeverage where
  encCBOR (MaxPledgeLeverage m) = encodeNullStrictMaybe encCBOR m
instance DecCBOR MaxPledgeLeverage where
  decCBOR = MaxPledgeLeverage <$> decodeNullStrictMaybe decCBOR
```
NOT a plain `NonNegativeInterval` — it is `StrictMaybe NonNegativeInterval`,
because the parameter is semantically optional even once "set" in current
PParams (pre-Dijkstra eras are defined as `SNothing` = uncapped leverage).
`encodeNullStrictMaybe`/`decodeNullStrictMaybe`
(`libs/cardano-ledger-binary/.../Encoder.hs:340-343`,
`.../Decoder.hs:635-641`): `SNothing -> CBOR null (0xf6)`; `SJust x ->` encode
`x` directly, no extra wrapper/tag beyond `x`'s own instance. Confirmed by
upstream's own CHANGELOG (`eras/dijkstra/impl/CHANGELOG.md:8`): *"Add the
`maxPledgeLeverage` protocol parameter (serializes as `nonnegative_interval /
nil`)"*.

`NonNegativeInterval = BoundedRatio NonNegativeInterval Word64`, bounds
`[0%1, maxBound%1]` (BaseTypes.hs:468-485). Encodes via `encodeRatioWithTag`
= **tag 30 + array(2)[numerator, denominator]**, both as the underlying
integral type (Word64 here). Encode side (`EncCBOR (BoundedRatio b a)`,
BaseTypes.hs:404, empty instance body) is UNCONDITIONAL — always goes through
`Plain.toCBOR` default method, tag 30 always, not PV-gated. Decode gains a
PV>=12 branch (`decodeIntegralRational`, `DecCBOR.hs:214-223`): still asserts
tag 30, decodes as a list via `decodeList` (tolerates definite+indefinite
framing, unlike old `decodeListLenOf`-based path), explicit
"Denominator cannot be zero" check. Range validated via `boundRational` in
BOTH pre/post-PV12 paths.

Byte example: `SNothing` → key+value = `18 26 F6`. `SJust 3%1` → `18 26 D8 1E 82 03 01`.

## Key 39 `minPoolMargin` (CIP-0023, per CHANGELOG.md:22 — NOT CIP-50)

`dppMinPoolMargin :: THKD (...) f UnitInterval` — plain `UnitInterval`, no
`StrictMaybe` wrapper. Default `THKD minBound` = `0%1` (DijkstraPParams.hs:168,
687). `PParam` record `ppuTag = 39` at DijkstraPParams.hs:560-572.

`UnitInterval = BoundedRatio UnitInterval Word64`, bounds `[0%1, 1%1]`
(BaseTypes.hs:543-556) — same tag-30 `[num,den]` CBOR shape as
NonNegativeInterval, just a tighter bound (rejects n>d after reduction).
Byte example: `1/10` → `18 27 D8 1E 82 01 0A`.

Both keys wired through the fully GENERIC `PParamsUpdate` sparse-map
EncCBOR/DecCBOR (`Core/PParams.hs:249-298`, no per-field override, `ppEraDecoder
= Nothing` for both) — `encodeMapLen count` (always-definite header, count =
number of SJust fields present) `<> [encodeWord ppuTag <> encCBOR value | ...]`.
Same mechanism as every other PPU key; nothing special needed on the
map-framing side, only the per-type EncCBOR/DecCBOR shape above.

## Q3: AuxData key 5 (PlutusV4 script list) — LIVE, not a TODO, but capped by eraMaxLanguage

`eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/TxAuxData.hs` (full file, no
overrides beyond typeclass wiring): `type TxAuxData DijkstraEra =
AlonzoTxAuxData DijkstraEra` — verbatim reuse, confirming the question's
premise.

Unlike the witness-set case (dugite #1000: `AlonzoTxWits` literal `-- TODO:
Add plutus_v4_script at index 8`), **`AlonzoTxAuxData`'s key 5 for PlutusV4
IS implemented**, `eras/alonzo/impl/src/Cardano/Ledger/Alonzo/TxAuxData.hs`:
- EncCBOR (line 167): `Omit isNothing (Key 5 $ E (maybe mempty encCBOR) (Map.lookup PlutusV4 atadrPlutusScripts))`
- DecCBOR (lines 280, 299): `5 -> decodeAddPlutus PlutusV4` /
  `auxDataField 5 = fieldA (addPlutusScripts PlutusV4) (D (guardPlutus PlutusV4 >> decCBOR))`
- `guardPlutus` (`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Language.hs:639-647`)
  is a DECODE-TIME PROTOCOL-VERSION gate only (`PlutusV4 -> natVersion @12`),
  NOT an era-capability check — any era decoding at PV>=12 accepts key 5 at
  the raw CBOR layer.

**Caveat (found, not assumed) — Dijkstra's OWN `eraMaxLanguage` is still
capped at `PlutusV3`**: `eras/dijkstra/impl/src/Cardano/Ledger/Dijkstra/Scripts.hs:469`,
`instance AlonzoEraScript DijkstraEra where ... eraMaxLanguage = PlutusV3`
— this is despite the SAME instance defining a full `DijkstraPlutusV4`
constructor and `mkPlutusScript`/`withPlutusScript` arms for it (lines
462-481). `eraMaxLanguage` is consumed by `getAlonzoTxAuxDataScripts`
(`Alonzo/TxAuxData.hs:192-208`): `[lang | lang <- [PlutusV1 ..
eraMaxLanguage @era], ...]` — this is the accessor that reconstructs "all
scripts attached to this tx" (used for hashing/witnessing/UTXOW). Net effect:
a PlutusV4 script placed under aux-data key 5 will CBOR-decode successfully
into the internal map, but `getAlonzoTxAuxDataScripts` will silently exclude
it from the era-visible script list as of this pinned commit — the wire slot
exists and parses, but Dijkstra's ledger-rule layer does not yet surface it.
Functionally analogous outcome to the witness-set TODO (V4-via-this-path is
not yet load-bearing), via a different mechanism (silent enumeration drop,
not a missing CBOR key).

See also [[plutus-v4-dijkstra-witness-set-and-scriptcontext-status]] for the
witness-set (key 8) side of this comparison.
