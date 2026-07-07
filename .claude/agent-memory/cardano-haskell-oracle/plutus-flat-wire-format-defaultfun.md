---
name: plutus-flat-wire-format-defaultfun
description: Authoritative DefaultFun flat wire-ID table (explicit instance, NOT declaration order), constr/case version gating, checkScope recursion gap, Program trailing-byte rules, BLS12-381 constant decode-always-fails behavior. Source IntersectMBO/plutus tag 1.65.0.0.
metadata:
  type: reference
---

Verified 2026-07-04 against IntersectMBO/plutus tag `1.65.0.0` (latest release at
the time). All line numbers refer to that tag.

## 1. DefaultFun flat wire IDs are an EXPLICIT hand-written mapping, NOT declaration order

File: `plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs`, `instance Flat DefaultFun`
(encode `\case` at line 2513-2616, decode `go` at 2618-2721).

The `data DefaultFun` declaration (line 88) groups constructors by *topic* (all Data-related
builtins together, all crypto-verify builtins together, etc). The `Flat` instance assigns wire
tags in *addition order* (new builtins always appended at the end), which diverges from
declaration order in two places:

- `VerifyEcdsaSecp256k1Signature`/`VerifySchnorrSecp256k1Signature` are declared right after
  `VerifyEd25519Signature` (~position 22-23) but their WIRE ids are **52, 53** (added later,
  appended after the Data/Pair block).
- Within each BLS12-381 G1/G2 group, the data declaration lists `hashToGroup` before
  `compress`/`uncompress`, but the wire order swaps them.

**Full verified wire-ID table (0-100):**
```
0 AddInteger .. 9 LessThanEqualsInteger
10 AppendByteString .. 17 LessThanEqualsByteString
18 Sha2_256, 19 Sha3_256, 20 Blake2b_256, 21 VerifyEd25519Signature
22 AppendString, 23 EqualsString, 24 EncodeUtf8, 25 DecodeUtf8
26 IfThenElse, 27 ChooseUnit, 28 Trace, 29 FstPair, 30 SndPair
31 ChooseList, 32 MkCons, 33 HeadList, 34 TailList, 35 NullList
36 ChooseData, 37 ConstrData, 38 MapData, 39 ListData, 40 IData, 41 BData
42 UnConstrData, 43 UnMapData, 44 UnListData, 45 UnIData, 46 UnBData
47 EqualsData, 48 MkPairData, 49 MkNilData, 50 MkNilPairData
51 SerialiseData
52 VerifyEcdsaSecp256k1Signature, 53 VerifySchnorrSecp256k1Signature
54 Bls12_381_G1_add, 55 G1_neg, 56 G1_scalarMul, 57 G1_equal,
58 G1_compress, 59 G1_uncompress, 60 G1_hashToGroup
61 Bls12_381_G2_add, 62 G2_neg, 63 G2_scalarMul, 64 G2_equal,
65 G2_compress, 66 G2_uncompress, 67 G2_hashToGroup
68 millerLoop, 69 mulMlResult, 70 finalVerify
71 Keccak_256, 72 Blake2b_224
73 IntegerToByteString, 74 ByteStringToInteger
75 AndByteString, 76 OrByteString, 77 XorByteString, 78 ComplementByteString
79 ReadBit, 80 WriteBits, 81 ReplicateByte
82 ShiftByteString, 83 RotateByteString, 84 CountSetBits, 85 FindFirstSetBit
86 Ripemd_160, 87 ExpModInteger
88 DropList
89 LengthOfArray, 90 ListToArray, 91 IndexArray
92 Bls12_381_G1_multiScalarMul, 93 G2_multiScalarMul
94 InsertCoin, 95 LookupCoin, 96 UnionValue, 97 ValueContains,
98 ValueData, 99 UnValueData, 100 ScaleValue
```
(89-100 matches what `.claude/agent-memory/tech-lead/uplc-builtin-flat-id-mismatch.md`
already recorded from the #761 incident; 58-60/65-67 compress/uncompress/hashToGroup order
also matches that memory and is now re-confirmed against current upstream source.)

`builtinTagWidth = 7` bits (line 2503-2504); decode failure message for unknown tag:
`"Failed to decode builtin tag, got: " ++ show t` (line 2721).

## 2. constr/case (term tags 8/9) version gate is a DECODE-time Get failure

File: `plutus-core/untyped-plutus-core/src/UntypedPlutusCore/Core/Instance/Flat.hs`,
`decodeTerm` (takes `Version` as first arg, threaded from `decodeProgram`'s own decoded
`v :: Version` field — i.e. the version comes from the program's OWN header, decoded
just before the term).

```haskell
handleTerm 8 = do
  unless (version >= PLC.plcVersion110) $
    fail $ "'constr' is not allowed before version 1.1.0, this program has version: "
      ++ (show $ pretty version)
  ...
handleTerm 9 = do
  unless (version >= PLC.plcVersion110) $
    fail $ "'case' is not allowed before version 1.1.0, this program has version: " ++ ...
```
`plcVersion110 = Version 1 1 0` (`PlutusCore/Version.hs` line 66-67); `Version` derives `Ord`
so comparison is lexicographic (major, minor, patch). This is a hard decode-time `Get`
failure (MonadFail), i.e. a **phase-1** deserialisation error, not a CEK/phase-2 error.

## 3. checkScope does NOT recurse into Constr/Case subterms — important gap

File: `plutus-core/untyped-plutus-core/src/UntypedPlutusCore/Check/Scope.hs` (56 lines total,
`checkScope`, called by `PlutusLedgerApi.Common.Eval.mkTermToEvaluate` via
`through (liftEither . first DeBruijnError . UPLC.checkScope) appliedT`, i.e. run on the
WHOLE (already-argument-applied) term BEFORE the CEK machine starts — this makes a free
variable violation a **phase-2 (collateral) failure** (`DeBruijnError FreeVariableError`
inside `EvaluationError`, thrown from within `evaluateScriptRestricting`/`evaluateScriptCounting`).

```haskell
checkScope = go 0
  where
    go !lvl = \case
      Var _ n -> ... unless (i>0 && i<=lvl) $ throwError (FreeIndex i)
      LamAbs _ binder t -> ... go (lvl+1) t
      Apply _ t1 t2 -> go lvl t1 >> go lvl t2
      Force _ t -> go lvl t
      Delay _ t -> go lvl t
      _ -> pure ()   -- <-- Constant, Builtin, Error, Constr, Case ALL fall here
```
So: a free/out-of-scope variable inside an **unforced `Delay`** IS caught eagerly (Delay is
explicitly recursed) — confirming the check is eager/whole-term for the classic lambda-calculus
spine. But a free variable buried inside a `Constr` field or a `Case` scrutinee/branch is
**NOT** traversed by this pass at all (wildcard `_ -> pure ()` matches Constr/Case without
recursing into their subterms). Any such free variable would only surface later, lazily, if
and when the CEK machine actually evaluates that particular Constr field / Case branch.
This is a genuine, non-obvious divergence between "eager whole-term" and "eager whole-spine
minus Constr/Case payloads" — worth flagging explicitly if Dugite's scope-checker assumes full
recursion into every constructor.

## 4. Program trailing-byte / full-consumption rules are TWO SEPARATE, differently-gated checks

**(A) Flat-layer, unconditional, all Plutus versions:** `PlutusCore.Flat.Run.unflatWith` = `unflatRawWith (postAlignedDecoder dec)`,
and `strictDecoder` (`plutus-core/flat/src/PlutusCore/Flat/Decoder/Run.hs` line 19-24):
```haskell
strictDecoder get bs usedBits =
  ... if ptr' /= endPtr || o' /= 0
      then tooMuchSpace endPtr s'
      else return a
```
Doc comment: "returns either the decoded value or an error (if the input buffer is not fully
consumed)". This means: the flat-encoded blob (term + trailing `Filler`) must consume EXACTLY
every byte handed to it, byte-aligned (`o' == 0`) — this check is unconditional, applies to
every Plutus Ledger Language. The `Filler` itself (`PlutusCore/Flat/Filler.hs`) is
`FillerBit Filler | FillerEnd`, generically-derived Flat instance — i.e. a run of 0-bits
terminated by exactly one 1-bit (matches `0*1`), consumed via `postAlignedDecoder` after
the term.

**(B) Ledger-API layer, version-gated:** `PlutusLedgerApi.Common.SerialisedScript.deserialiseScript`
(line 254-266) calls `CBOR.deserialiseFromBytes (scriptCBORDecoder ll pv)` which returns
`(remderBS, dScript)` — leftover bytes in the OUTER buffer after the single CBOR item (a CBOR
byte-string wrapping the flat blob) has been consumed:
```haskell
when (ll /= PlutusV1 && ll /= PlutusV2 && remderBS /= mempty) $
  throwing _ScriptDecodeError $ RemainderError remderBS
```
So trailing bytes AFTER the CBOR-bstr-wrapped script are silently ignored for PlutusV1/V2
(legacy quirk, explicitly commented as intentional backward-compat) and rejected
(`RemainderError`) from PlutusV3 onward. `scriptCBORDecoder` itself calls
`decodeViaFlatWith flatDecoder` (`Codec/Extras/SerialiseViaFlat.hs` line 28-32), which does
`CBOR.decodeBytes` then `Flat.unflatWith decoder bs` — i.e. check (A) above is applied to the
bytes INSIDE the CBOR bstr regardless of ll/pv; check (B) is the separate, version-gated check
for bytes outside/after that CBOR item.

## 5. BLS12-381 G1/G2/MlResult: universe tag OK, value decode ALWAYS fails, empty list decodes fine

File: `plutus-core/plutus-core/src/PlutusCore/Default/Universe.hs` — universe tags via
`encodeUni`/`decodeUni` (line 977-990): `DefaultUniBLS12_381_G1_Element = [9]`,
`_G2_Element = [10]`, `_MlResult = [11]`. These are accepted at the TYPE level (`decodeKindedUniFlat`
in `PlutusCore/FlatInstances.hs` line 128-134, used for both bare types and compound types like
`list bls12_381_G1_element` via `DefaultUniApply`).

But the underlying Haskell types' own `Flat` instances always fail on decode:
- `plutus-core/plutus-core/src/PlutusCore/Crypto/BLS12_381/G1.hs` (`instance Flat Element`):
  `decode = fail "Flat decoding is not supported for objects of type bls12_381_G1_element: use bls12_381_G1_uncompress on a bytestring instead."`
- G2.hs: identical pattern, `bls12_381_G2_element`/`bls12_381_G2_uncompress` in the message.
- `Pairing.hs` (`instance Flat MlResult`): `decode = fail "Flat decoding is not supported for objects of type bls12_381_mlresult"`.

`Some (ValueOf uni)` decode (`PlutusCore/FlatInstances.hs` line 153-161) decodes the type tag
first, then does `bring (Proxy @Flat) uni decode` to invoke the underlying type's decoder — so
any constant of exactly `bls12_381_G1_element`/`_G2_element`/`_mlresult` unconditionally fails
to decode, REGARDLESS of the actual bytes.

**Empty-list edge case confirmed:** the generic list Flat instance
(`plutus-core/flat/src/PlutusCore/Flat/Instances/Base.hs` line 627,
`instance {-# OVERLAPPABLE #-} Flat a => Flat [a]`) uses the default/generic Cons-or-Nil
single-bit-tag encoding (doctest: `test ([]::[Bool]) == (True,1,"0")` — one bit, no element
decode call at all). So `(con (list bls12_381_G1_element) [])` decodes as `[]` successfully
(reads one Nil-tag bit, never calls `Element`'s always-failing decoder), while any list with
>=1 element of that type unconditionally fails.

## 6. ByteString flat chunk encoding: confirmed max 255, 0-terminated

`plutus-core/flat/src/PlutusCore/Flat/Instances/ByteString.hs` (Haddock on `instance Flat
B.ByteString`, line 21-63): "byte-aligned sequence of blocks of up to 255 elements, with every
block preceded by the count of the elements in the block and a final 0-length block." Example:
`tst (B.pack [11,22,33]) == (True,48,[1,3,11,22,33,0])` — pre-alignment filler byte, then
length-byte, then payload bytes, repeated, terminated by a `0` length byte. Applies identically
to lazy and short ByteString (all delegate to the same chunked encoder).
