---
name: native-script-hash-original-bytes-not-reencode
description: hashScript/SafeToHash/MemoBytes chain proves ScriptHash is blake2b_224(prefix||ORIGINAL wire bytes), never a re-encode; covers Timelock CBOR decoder laxity (indefinite-length arrays + non-minimal SlotNo ints accepted) and TxOut scriptRef tag(24) nested-CBOR unwrapping
type: reference
---

# Native-script hashing: original bytes, not canonical re-encode

Verified live 2026-07-06 against IntersectMBO/cardano-ledger `master` (Conway
era, same code path used by cardano-node 11.0.1). This directly disproves a
dugite implementation that decodes a Timelock/MultiSig script AST and then
canonically RE-ENCODES it before hashing — Haskell never re-encodes.

## 1. `hashScript` / `EraScript` — `libs/cardano-ledger-core/src/Cardano/Ledger/Core.hs`

```haskell
-- | Compute `ScriptHash` of a `Script` for a particular era.
hashScript :: forall era. EraScript era => Script era -> ScriptHash
hashScript =
  ScriptHash
    . Hash.castHash
    . Hash.hashWith
      (\x -> scriptPrefixTag @era x <> originalBytes x)
```
Doc comment on the `EraScript` class (same file, ~line 544): "You must
understand the role of SafeToHash and scriptPrefixTag to make new instances
... **The safeToHash constraint ensures that Scripts are never reserialised.**"
`SafeToHash (Script era)` is a hard superclass constraint of `EraScript`.

Plutus has its own equivalent, `hashPlutusScript` in
`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Language.hs` (~line 159),
doc-commented "equivalent to `Cardano.Ledger.Core.hashScript`, except
restricted to Plutus scripts" — same `prefix <> originalBytes` shape.

## 2. `SafeToHash` + `MemoBytes` — the original-bytes capture mechanism

Class defined in `libs/cardano-ledger-core/src/Cardano/Ledger/Hashes.hs` (~line 373):
```haskell
class SafeToHash t where
  originalBytes :: t -> ByteString
```
The universal instance, in
`libs/cardano-ledger-core/src/Cardano/Ledger/MemoBytes/Internal.hs` (line 194):
```haskell
instance SafeToHash (MemoBytes t) where
  originalBytes = fromShort . mbBytes
```
`MemoBytes` (same file, line 111):
```haskell
data MemoBytes t = MemoBytes
  { mbRawType :: !t
  , mbBytes :: ShortByteString   -- <- the actual wire bytes as received
  , mbHash :: SafeHash (MemoHashIndex t)
  }
```
Captured during decode via the `Annotator`/`withSlice` mechanism (line 176-179,
same file):
```haskell
instance (Typeable t, DecCBOR (Annotator t)) => DecCBOR (Annotator (MemoBytes t)) where
  decCBOR = do
    (Annotator getT, Annotator getBytes) <- withSlice decCBOR
    pure $ Annotator $ \fullBytes -> mkMemoBytesForce <$> getT fullBytes <*> getBytes fullBytes
```
`withSlice` in `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/Annotated.hs`
(line 227) uses `annotatedDecoder` → `decodeWithByteSpan` → a `ByteSpan` of raw
byte offsets (`ByteSpan !ByteOffset !ByteOffset`, line 79) and slices the
**original input ByteString** at those offsets (line 73-74: `slice bytes
(ByteSpan start end)`). This is a byte-offset slice of the wire input, not a
re-serialization — non-canonical/non-minimal input bytes are preserved
verbatim.

`Timelock`, `eras/allegra/impl/src/Cardano/Ledger/Allegra/Scripts.hs` (line 253):
```haskell
newtype Timelock era = MkTimelock (MemoBytes (TimelockRaw era))
  deriving newtype (ToCBOR, NFData, SafeToHash)
```
So `originalBytes` on a `Timelock` is exactly `fromShort . mbBytes` of its own
`MemoBytes` — the bytes consumed for that specific script term at decode time.

## 3. Timelock CBOR decoder laxity — same file, lines 179-246

- Outer sum-constructor array (`decRaw`/`Summands "TimelockRaw"`) resolves
  through `decodeRecordSum` → `decodeListLike` in
  `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/Decoder.hs`
  (line 648, docstring: "Use this decoder for any list like structure that
  accepts fixed or variable list length encoding"). Implementation
  (`decodeListLikeT`, line 669-677) calls `decodeListLenOrIndef` and handles
  BOTH `Just n` (definite) and `Nothing` (indefinite, consumes trailing
  break-or). **Both wire forms are valid input** for every `Summands`-based
  sum type in cardano-ledger, including `TimelockRaw`, `AlonzoScript`, etc.
- `SlotNo` (`invalidBefore`/`invalidHereafter`, i.e. `RequireTimeStart` /
  `RequireTimeExpire`) is `newtype SlotNo = SlotNo Word64` (IntersectMBO/cardano-base
  `cardano-slotting/src/Cardano/Slotting/Slot.hs`). Its `DecCBOR` instance,
  `libs/cardano-ledger-core's DecCBOR.hs` (well, actually
  `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/DecCBOR.hs`
  line 580): `decCBOR = fromPlainDecoder Serialise.decode` — the generic
  `Codec.Serialise`/`cborg` Word64 decoder, i.e. plain `decodeWord64`
  (`DecCBOR Word64` instance, same file line 161: `decCBOR = decodeWord64`),
  **not** the `decodeWord64Canonical` variant that exists alongside it
  (`Decoder.hs` line 1627 vs 1631). Plain CBOR uint decode accepts ANY valid
  additional-info length encoding of a given value (direct 0-23, uint8,
  uint16, uint32, uint64) — it does not reject non-minimal forms. So
  `0x1a00000005` (4-byte uint32 form of 5) decodes to `SlotNo 5` exactly like
  the canonical `0x05`, and the surrounding `mbBytes` slice preserves whichever
  form was actually on the wire.
- Conclusion for dugite: a **valid** (per Haskell) native script can arrive
  with an indefinite-length constructor array and/or non-minimal SlotNo
  integers, and Haskell's decoder accepts it and hashes the AS-RECEIVED bytes.
  A canonical re-encoder in dugite would silently produce a DIFFERENT
  ScriptHash than upstream for such inputs — a byte-exact divergence bug,
  exactly the class the dugite engineer suspected.
- Real-world precedent: NONE known/needed to assume malicious intent — but
  also note Haskell's own encoder never emits indefinite-length arrays for
  Timelock sub-lists: `encodeStrictSeq`/`encodeSeq` in
  `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding/Encoder.hs`
  bottom out in `encodeFoldableAsDefLenList` (line 349) — always definite
  length. So legitimate cardano-cli/cardano-api/cardano-serialization-lib
  output is always definite-length + minimal ints; the lax-decode paths above
  are a purely defensive/adversarial concern (someone hand-crafting a tx),
  not something you'll see from real tooling.

## 4. Witness-set vs reference-script TxOut — same mechanism, verified both ends

`AlonzoScript` (used as `Script era` from Alonzo through Conway — Conway does
NOT define its own; `eras/conway/impl/src/Cardano/Ledger/Conway/Scripts.hs`
line 72-81: `instance EraScript ConwayEra where type Script ConwayEra =
AlonzoScript ConwayEra; type NativeScript ConwayEra = Timelock ConwayEra;
scriptPrefixTag = alonzoScriptPrefixTag`):

`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Scripts.hs` line 475-520:
```haskell
data AlonzoScript era = NativeScript !(NativeScript era) | PlutusScript !(PlutusScript era)

-- | Both constructors know their original bytes
instance (SafeToHash (PlutusScript era), SafeToHash (NativeScript era)) => SafeToHash (AlonzoScript era) where
  originalBytes (NativeScript t) = originalBytes t
  originalBytes (PlutusScript plutus) = originalBytes plutus
```
Critically, `AlonzoScript` is a **plain sum type, not itself wrapped in a
`MemoBytes`** — its `SafeToHash` instance is hand-written to delegate straight
to the INNER constructor's own `originalBytes`. Meanwhile its `EncCBOR`
instance (line 690) DOES wrap the inner script when serialized as a whole:
`NativeScript i -> Sum NativeScript 0 !> To i` → wire form `[0, <timelock
CBOR>]`. So there are two different notions of "this script's bytes": (a) the
`[0, <inner>]`-wrapped bytes you'd get from re-encoding the `AlonzoScript`
value as a standalone term, and (b) `originalBytes` used for hashing, which
is only the INNER Timelock's own captured span — excluding the `[0, ...]`
wrapper entirely.

Reference scripts (Babbage+/Conway `TxOut`) go through this exact same
`AlonzoScript`/`Timelock` decode+hash path, with one extra layer of framing to
strip first: `eras/babbage/impl/src/Cardano/Ledger/Babbage/TxOut.hs`, the
`scriptRef` field (key 3 in the sparse-keyed `TxOut` map, line 636) decodes
via `decodeCIC "Script"` (line 765):
```haskell
decodeCIC :: DecCBOR (Annotator b) => T.Text -> Decoder s b
decodeCIC s = do
  version <- getDecoderVersion
  lbs <- decodeNestedCborBytes
  case decodeFullAnnotator version s decCBOR (LBS.fromStrict lbs) of ...
```
`decodeNestedCborBytes`, `libs/cardano-ledger-binary/.../Binary/Decoding.hs`
line 238: `decodeNestedCborTag >> decodeBytes` where `decodeNestedCborTag`
(line 213-217) checks the CBOR tag number is literally `24` (`DecoderErrorUnknownTag`
otherwise) then `decodeBytes` returns the inner bstr payload UNCHANGED (doc
comment line 237: "yields the inner ... unchanged"). The unwrapped bytes are
then fed to the ordinary `AlonzoScript` `Annotator` decoder — which recurses
into `Timelock`'s own `MemoBytes`/`withSlice` exactly as in the witness-set
case. Encode side is symmetric: `encodeNestedCborBytes x = encodeTag 24 <>
encCBOR x` (`Binary/Encoding.hs` line 92).

**Conclusion**: `hashScript` on a reference script strips BOTH the outer
CBOR tag(24)/bstr framing (via `decodeNestedCborBytes`) AND the `[0, <bytes>]`
`AlonzoScript`-level sum-type discriminant (via `SafeToHash (AlonzoScript
era)`'s direct delegation to the inner constructor) — leaving only the
Timelock's own original wire bytes, byte-for-byte identical treatment to the
witness-set case. hashScript is 100% agnostic to where the `Script era` value
came from; it only ever looks at the innermost decoded term's own captured
byte span.

## 5. Script prefix-tag table (byte prepended before hashing)

`nativeMultiSigTag`, `eras/shelley/impl/src/Cardano/Ledger/Shelley/Scripts.hs`
line 136-137: `nativeMultiSigTag = "\00"` (0x00). Plutus tags,
`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Language.hs`:
`plutusLanguageTag` instances at lines 473/493/513/534 → PlutusV1=0x01,
PlutusV2=0x02, PlutusV3=0x03, PlutusV4=0x04. `Language` sum type (line 224-228)
already has a `PlutusV4` constructor defined in current master — NOT active in
any released/mainnet era as of Conway/PV11, just forward-declared plumbing.
`alonzoScriptPrefixTag` (`Alonzo/Scripts.hs` line 539-545) is the single
dispatch function used by every era from Alonzo through Conway
(`scriptPrefixTag = alonzoScriptPrefixTag` in both `Alonzo/Scripts.hs` line
531 and `Conway/Scripts.hs` line 81).

## Dugite relevance
- Any dugite code path that computes `ScriptHash` by decoding a Timelock/
  MultiSig into an AST and re-encoding it canonically is WRONG whenever the
  wire input isn't already canonical (indefinite-length ctor arrays,
  non-minimal SlotNo ints). Fix: capture the exact byte range consumed
  during decode (mirror `withSlice`/`MemoBytes` — i.e. record decoder
  start/end position around each script-witness or reference-script decode)
  and hash `prefix_byte || raw_captured_bytes`, never a re-serialize.
  See also [[cardano-ledger-types-wire-format]] for the summary prefix table
  (less detailed than this entry).
