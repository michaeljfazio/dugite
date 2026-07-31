---
name: metadatum-codec-definite-indefinite-gates
description: Metadata.hs decodeMetadatum/encodeMetadatum exact definite/indefinite CBOR gates — TypeTag rejection, checkSizes>PV2, byte-chunk PV12 leniency (text never), encoder always-definite via raw encodeListLen/encodeMapLen
type: reference
---

Verified live against IntersectMBO/cardano-ledger master @ `58ba7795273f9301a9a198930e50a6ca1ee85238` (2026-07-31).

File: `libs/cardano-ledger-core/src/Cardano/Ledger/Metadata.hs`

## decodeMetadatum dispatch (peekTokenType)

Accepts BOTH definite and indefinite arms for all 3 compound-ish shapes:
`TypeBytes`/`TypeBytesIndef`, `TypeString`/`TypeStringIndef`,
`TypeListLen`/`TypeListLen64`/`TypeListLenIndef`,
`TypeMapLen`/`TypeMapLen64`/`TypeMapLenIndef`. Integer arms accepted:
`TypeUInt`, `TypeUInt64`, `TypeNInt`, `TypeNInt64` only — NOT `TypeTag`/
`TypeTag64` (falls to `_ -> decodeError`, so a tag-2/3 CBOR bignum is not a
valid metadatum even though it denotes an integer).

## 64-byte leaf bound

```haskell
dv <- getDecoderVersion
let checkSizes = dv > natVersion @2
```
`shelleyProtVer = natVersion @2` (Version.hs) — so checkSizes is false at
Shelley (PV2), true from Allegra (PV3) onward. For the *Indef arms the bound
is checked on the ALREADY-CONCATENATED result (`ba`/`x` after
`decodeBytesIndefLen`/`decodeStringIndefLen` fully drains to the break),
never per-chunk.

## Byte-chunk vs text-chunk definiteness — NOT symmetric

`decodeStringIndefLen`'s chunk decoder calls `decodeString` directly, which
is `fromPlainDecoder C.decodeString` — unconditional, definite-only, no
version gate, ever. A nested indefinite text chunk always fails.

`decodeBytesIndefLen`'s chunk decoder calls plain `decCBOR` at type
`ByteArray`, which resolves to `Cardano.Ledger.Binary.Decoding.Decoder.
decodeByteArray` (not a "plain" primitive):
```haskell
decodeByteArray =
  ifDecoderVersionAtLeast (natVersion @12) decodeByteArrayDefOrIndef decodeByteArrayDefinite
```
Below decoder version 12 this is definite-only (matches the text case).
**At decoder version >= 12 it also accepts one level of nested
`TypeBytesIndef` as a "chunk"** (recursively re-concatenating that nested
indefinite bytestring's own definite sub-chunks) — CHANGELOG.md
(cardano-ledger-binary) 1.9.0.0: "Make `decodeBytes` and `decodeByteArray`
handle indefinite length encoding"; MaxVersion bumped to 12 in 1.7.0.0.
Per dugite's own PV≥12-is-Dijkstra convention (see redeemers/datums memory),
this is currently unreached by any live/foreseeable era through Conway
(PV9-11) — so today the "chunks must be definite" claim holds in practice
for both bytes and text — but it is NOT a version-independent guarantee for
bytes, and dugite's #937 fix commit (a4396cbfd2) states the definite-chunk
rule as an unqualified fact ("chunks must themselves be definite, as in
Haskell") without the PV12 carve-out. Revisit when Dijkstra-era (PV12) work
starts (see #936 in dugite CLAUDE.md).

## encodeMetadatum — always definite, never the generic `encodeMap`/size-gated form

```haskell
encodeMetadatum (List xs) = encodeListLen (fromIntegral (length xs)) <> ...
encodeMetadatum (Map kvs) = encodeMapLen (fromIntegral (length kvs)) <> ...
```
`encodeListLen`/`encodeMapLen` (Encoder.hs) are raw `fromPlainEncoding
(C.encodeListLen/C.encodeMapLen e)` — unconditionally definite, no size
threshold, no version gate. This is a DIFFERENT function from the generic
`encodeMap` helper (also in Encoder.hs, used for withdrawals/PParamsUpdate/
voting-procedures/etc.) which DOES branch: `ifEncodingVersionAtLeast
(natVersion @2) (variableMapLenEncoding ...) (exactMapLenEncoding ...)` —
i.e. Shelley+ switches to indefinite above a size threshold. `encodeMetadatum`
never calls that helper, so `Metadatum::Map`/`List` must stay always-definite
regardless of entry count — matches dugite's own #932 pinning
("PlutusData::Map, nested Metadatum::Map ... pinned always-definite").
