---
name: native-script-hash-memobytes-safetohash
description: hashScript/SafeToHash/MemoBytes exact mechanics for native (Timelock) scripts — original wire bytes, never re-encoded; Timelock decoder tolerance; witness-set vs reference-script hashing identical
metadata:
  type: reference
---

Live-verified 2026-07-06 against `IntersectMBO/cardano-ledger` `master` (same code path as cardano-node 11.0.1, Conway era). Answers "does hashScript hash original bytes or re-encode the AST" definitively: **original bytes only, never a re-encode.**

## 1. hashScript never re-encodes

`libs/cardano-ledger-core/src/Cardano/Ledger/Core.hs`:
```haskell
hashScript :: forall era. EraScript era => Script era -> ScriptHash
hashScript =
  ScriptHash . Hash.castHash . Hash.hashWith
    (\x -> scriptPrefixTag @era x <> originalBytes x)
```
`EraScript` doc comment: "the safeToHash constraint ensures that Scripts are never reserialised." `SafeToHash (Script era)` is a hard superclass constraint — you cannot even construct an `EraScript` instance without it.

`SafeToHash` class (`libs/cardano-ledger-core/src/Cardano/Ledger/Hashes.hs` ~373): `class SafeToHash t where originalBytes :: t -> ByteString`.

Universal `MemoBytes` instance (`libs/cardano-ledger-core/src/Cardano/Ledger/MemoBytes/Internal.hs:194`):
```haskell
instance SafeToHash (MemoBytes t) where
  originalBytes = fromShort . mbBytes
```
`MemoBytes` (same file, line 111) stores `mbBytes :: ShortByteString` captured during decode via `Annotator`/`withSlice` (`libs/cardano-ledger-binary/.../Binary/Decoding/Annotated.hs:73-231`) — a literal `slice bytes (ByteSpan start end)` over the raw input wire bytes, NOT a re-serialization.

`Timelock era = MkTimelock (MemoBytes (TimelockRaw era))`, `deriving newtype SafeToHash` — `eras/allegra/impl/src/Cardano/Ledger/Allegra/Scripts.hs:253-255`.

## 2. Timelock decoder accepts non-canonical wire forms

Outer sum-array decode chain: `decodeRecordSum` → `decodeListLike`/`decodeListLikeT` (`libs/cardano-ledger-binary/.../Binary/Decoding/Decoder.hs:648-677`) calls `decodeListLenOrIndef` and explicitly branches `Just n` (definite) vs `Nothing` (indefinite — then consumes a trailing break byte). **Both forms are valid.**

`SlotNo` (used by `InvalidBefore`/`InvalidHereafter`) is `newtype SlotNo = SlotNo Word64`. Its `DecCBOR` instance bottoms out in the plain, non-canonical `decodeWord64` (NOT the distinct `decodeWord64Canonical` that also exists in the same module) — so a non-minimally-encoded uint (e.g. 4-byte `0x1a00000005` for value 5) decodes identically to the canonical 1-byte form, and the (different) original bytes get hashed as-is.

**Conclusion**: a Haskell-valid native script can legally arrive with an indefinite-length outer ctor array and/or non-minimal integer field encodings; whatever bytes it arrived in are exactly what gets hashed.

## 3. Witness-set script vs TxOut reference script — identical mechanism

`AlonzoScript era` (= `Script era` unchanged Alonzo→Conway; `eraScriptPrefixTag`/`alonzoScriptPrefixTag` used unchanged across eras) is a plain (non-MemoBytes) sum type with a **hand-written** `SafeToHash` instance (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Scripts.hs:475-520`):
```haskell
instance (...) => SafeToHash (AlonzoScript era) where
  originalBytes (NativeScript t) = originalBytes t
  originalBytes (PlutusScript plutus) = originalBytes plutus
```
This delegates straight to the inner constructor's own captured bytes, **bypassing** `AlonzoScript`'s own `EncCBOR` sum-type wrapper (`NativeScript i -> Sum NativeScript 0 !> To i`, i.e. the `[0, <bytes>]` wire wrapper) entirely.

Reference scripts (Babbage+/Conway `TxOut` key 3, `scriptRef`) add one outer framing layer that is stripped before reaching this same path: `eras/babbage/impl/src/Cardano/Ledger/Babbage/TxOut.hs` `decodeCIC` calls `decodeNestedCborBytes` (`libs/cardano-ledger-binary/.../Binary/Decoding.hs:213-240` — checks CBOR tag 24, returns the inner bstr **unchanged**), then decodes those bytes through the ordinary `AlonzoScript` `Annotator`, which recurses into `Timelock`'s own `MemoBytes`/`withSlice` exactly as in the witness-set case. Encode side is symmetric: `encodeNestedCborBytes x = encodeTag 24 <> encCBOR x`.

**So**: `hashScript` on a reference native script strips BOTH the tag(24)/bstr framing AND the `[0, <bytes>]` sum-discriminant wrapper, leaving only the Timelock's own captured span — byte-identical treatment to the witness-set case. `hashScript` is entirely agnostic to where the `Script era` value came from (witness vs UTxO-stored reference script vs anywhere else).

## 4. Prefix-byte table

`nativeMultiSigTag = "\00"` (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Scripts.hs:136-137`). Plutus tags in `libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/Language.hs`: V1=0x01 (line 473), V2=0x02 (493), V3=0x03 (513), V4=0x04 (534, forward-declared, not active in any released era as of 11.0.1). One dispatch function (`alonzoScriptPrefixTag`) used unchanged Alonzo→Conway.

## 5. Real-world precedent for non-canonical native scripts

None known/observed on mainnet/preview/preprod — Haskell's own encoder (`encodeFoldableAsDefLenList`, `libs/cardano-ledger-binary/.../Encoder.hs:349`) always emits definite-length, and cardano-cli/cardano-api/cardano-serialization-lib all emit canonical minimal-int definite-length CBOR. This is a defensive/adversarial-input concern, not an observed-on-chain pattern — but per [[feedback_haskell_byte_exact_only]], a divergent ScriptHash on ANY Haskell-accepted input (even hand-crafted) is consensus-breaking, so tolerance must still match exactly.

Companion dugite-side finding: [[project_dugite_native_script_hash_audit_2026_07_06]].
