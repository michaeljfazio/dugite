---
name: variable-length-cbor-framing-and-blockbody-hash-over-original-bytes
description: lengthThreshold=23 variableListLenEncoding routing map (which container encoders actually call it vs. reimplement it), plus decisive proof AlonzoBlockBody hashes withSlice-captured ORIGINAL bytes, not a re-encoding
type: reference
---

Pinned to cardano-ledger master @ `58ba7795273f9301a9a198930e50a6ca1ee85238` (2026-08-01).

## lengthThreshold / variableListLenEncoding

`libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding/Encoder.hs`:

```haskell
lengthThreshold :: Int
lengthThreshold = 23

variableListLenEncoding :: Int -> Encoding -> Encoding
variableListLenEncoding len contents =
  if len <= lengthThreshold
    then exactListLenEncoding len contents
    else encodeListLenIndef <> contents <> encodeBreak
{-# INLINE variableListLenEncoding #-}
```
(lines 442-468). Identical shape `variableMapLenEncoding` exists for maps (lines 432-438), same `lengthThreshold`.

**Routing map — NOT all container encoders literally call `variableListLenEncoding`,
even though all end up with equivalent wire semantics (definite <=23, indefinite >23):**

| Encoder | Calls `variableListLenEncoding`? | Notes |
|---|---|---|
| `encodeFoldableEncoder` | YES, directly (line 364) | `encodeFoldableEncoder encoder xs = variableListLenEncoding len contents` |
| `encodeSeq` | YES, directly (line 512-513) | |
| `encodeStrictSeq` | YES, via `encodeSeq . SSeq.fromStrict` (line 516-518) | one hop |
| `encodeSet` | ONLY for PV>=2 branch | PV<2 branch uses `exactListLenEncoding` directly (always definite, never indefinite regardless of size) — see PV table below |
| `encodeList` (`[a]`) | **NO** — never calls it | Has its own parallel impl `encodeFoldableAsIndefLenList` / `encodeFoldableAsDefLenList` (lines 351-359, used at 499-508), justified by a comment: avoids computing full list length via `drop lengthThreshold xs` (laziness-friendly for possibly-large/infinite lists). Wire-format-equivalent to `variableListLenEncoding` with the same threshold=23, but NOT the same code path. Do not claim "`encodeList` routes through `variableListLenEncoding`" — it's false as literal code, true only as end-to-end wire semantics for PV>=2. |

`encodeSet` PV gating (lines 479-491, exact):
```haskell
encodeSet encodeValue f =
  let foldableEncoding = foldMap' encodeValue f
      varLenSetEncoding = variableListLenEncoding (Set.size f) foldableEncoding
   in ifEncodingVersionAtLeast (natVersion @2)
        ( ifEncodingVersionAtLeast (natVersion @9)
            (encodeTag setTag <> varLenSetEncoding)   -- PV>=9: tag + variable
            varLenSetEncoding                          -- PV in [2,9): variable, no tag
        )
        (encodeTag setTag <> exactListLenEncoding (Set.size f) foldableEncoding) -- PV<2: tag + ALWAYS exact/definite
```

`EncCBOR (Set.Set a) = encodeSet encCBOR` — `libs/cardano-ledger-binary/.../EncCBOR.hs` line ~317.
`EncCBOR [a] = encodeList encCBOR` — same file, line 299-300.
`EncCBOR (Seq.Seq a) = encodeSeq encCBOR`, `EncCBOR (SSeq.StrictSeq a) = encodeStrictSeq encCBOR` — same file, ~line 320-324.

`OSet.OSet` (`libs/cardano-data/src/Data/OSet/Strict.hs` line 112-113) is DIFFERENT from `Set.Set`:
```haskell
instance EncCBOR a => EncCBOR (OSet a) where
  encCBOR (OSet seq _set) = encodeTag setTag <> encodeStrictSeq encCBOR seq
```
Tag is **unconditional** (no `ifEncodingVersionAtLeast` gate at all, unlike `Set.Set`'s PV-gated tag), and length framing goes through `encodeStrictSeq` → `encodeSeq` → `variableListLenEncoding` (the "confirmed routes through" category).

`encodeStrictMaybe` uses literal arity, NOT the threshold mechanism at all (line 331-334):
```haskell
encodeStrictMaybe encodeValue = \case
  SNothing -> encodeListLen 0
  SJust x -> encodeListLen 1 <> encodeValue x
```

## Conway TxBody field routing (`eras/conway/impl/src/Cardano/Ledger/Conway/TxBody.hs`)

`ConwayTxBodyRaw` (lines 122-144): `ctbrSpendInputs/ctbrCollateralInputs/ctbrReferenceInputs :: Set TxIn`, `ctbrReqSignerHashes :: Set (KeyHash Guard)` → `encodeSet` path. `ctbrOutputs :: StrictSeq (Sized (TxOut era))` → `encodeStrictSeq` path. `ctbrCerts, ctbrProposalProcedures :: OSet.OSet _` → `OSet`'s own instance (unconditional tag + `encodeStrictSeq`).

`encodeTxBodyRaw` (lines 590-618) — the `Key n (To field)` DSL: `To :: EncCBOR a => a -> Encode (Closed Dense) a` and `encodeClosed (To x) = encCBOR x` (`libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding/Coders.hs` lines 132, 213) — so `Key 1 (To ctbrOutputs)` etc. dispatch straight to the field's own `EncCBOR` instance, confirming the routing table above applies unchanged through the sparse-encoding DSL.

## CRITICAL: block-body hash is over ORIGINAL received bytes, never a re-encoding

`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/BlockBody/Internal.hs`:

Forge side (lines 133-157) builds `txSeqBodies`/`txSeqWits` via
`serializeFoldablePreEncoded x = serialize version $ encodeFoldableEncoder encodePreEncoded x`
(so a Haskell-forged block with >23 txs uses indefinite framing for those two segments — `encodeFoldableEncoder` routes through `variableListLenEncoding`). `txSeqIsValids = serialize version $ encCBOR $ nonValidatingIndices txns` where `nonValidatingIndices :: ... -> [Int]` (line 267) — a plain list, so THIS segment goes through the `encodeList` code path (not `encodeFoldableEncoder`), though wire-equivalent.

Decode side — `DecCBOR (Annotator (AlonzoBlockBody era))` (lines 213-258), **verbatim**:
```haskell
decCBOR = do
    (bodies, bodiesAnn) <- withSlice decCBOR
    (wits, witsAnn) <- withSlice decCBOR
    ...
    (auxData, auxDataAnn) <- withSlice $ do
      auxDataMap <- decCBOR
      auxDataSeqDecoder bodiesLength auxDataMap
    (isValIdxs, isValAnn) <- withSlice decCBOR
    ...
    pure $
      AlonzoBlockBodyInternal
        <$> txns
        <*> (hashAlonzoSegWits <$> bodiesAnn <*> witsAnn <*> auxDataAnn <*> isValAnn)
        <*> bodiesAnn
        <*> witsAnn
        <*> auxDataAnn
        <*> isValAnn
```
`withSlice` captures the exact original byte range consumed by the inner decoder (standard `Annotator`/`ByteSpan` pattern used throughout cardano-ledger — same mechanism as native-script hashing, see [[native-script-hash-original-bytes-not-reencode]]). `hashAlonzoSegWits` (lines 188-211) is fed these four `Annotator`-wrapped ORIGINAL byte slices — it never touches the decoded `bodies`/`wits`/`isValIdxs` values themselves for hashing purposes, only for count/range validation.

**Consequence**: `abbHash` (the value checked against the block header's body-hash field) is computed from whatever bytes were actually on the wire, regardless of definite vs. indefinite array framing. A block emitter that uses DEFINITE array framing for a >23-tx segment (deviating from canonical Haskell's indefinite framing above the 23-element threshold) will still decode successfully under cardano-node (CBOR decoders accept both forms) and will produce a SELF-CONSISTENT, ACCEPTED block, PROVIDED the emitter also hashes its own header's body-hash field over the same actually-transmitted bytes (not a canonical re-encoding) — which is exactly the discipline dugite already follows for native scripts/datums (see linked memory). This is not a validation loophole cardano-node closes: there is no canonical-CBOR-only enforcement at this layer for Shelley+ eras' block-body segments.

Do not confuse this with claims about strict/definite-only encoders elsewhere (e.g. `PlutusData::Map`) — those are DIFFERENT instances pinned deliberately definite-only; this file's finding is specific to `AlonzoBlockBody`'s four hashed segments (bodies/wits/auxdata/isValid) shared by every block-body-carrying era from Alonzo through Conway.
