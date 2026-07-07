---
name: plutus-builtin-availability-gate
description: Exact builtinsIntroducedIn/builtinsAvailableIn map (batches 1-6), PV constants, and the deserialiseScript rejection mechanism for builtin/version availability, keyed on (PlutusLedgerLanguage, MajorProtocolVersion). Source IntersectMBO/plutus tag 1.65.0.0.
metadata:
  type: reference
---

Verified 2026-07-06 against IntersectMBO/plutus tag `1.65.0.0` (commit
`b2db512618df08bd696e8d9c4229effcede01169`) and IntersectMBO/cardano-ledger
master. This is the AVAILABILITY gate (which builtins exist at all in a
given LL/PV) — distinct from `plutus-builtin-semantics-variant-costing.md`
and `plutus-builtins-adversarial-audit.md`, which cover DENOTATION/costing
changes of builtins that are already available.

## 1. Canonical source: `builtinsIntroducedIn`

File: `plutus-ledger-api/src/PlutusLedgerApi/Common/Versions.hs`. Batches
(lines 152-303, all marked `-- DO NOT CHANGE THIS.` except batch6 which is
still open for the *next* HF), then `builtinsIntroducedIn` (line 310-330)
and `builtinsAvailableIn = collectUpTo . builtinsIntroducedIn` (line 335,
folds/unions all `Map.Map MajorProtocolVersion (Set DefaultFun)` entries
with key `<= thisPv` via `Map.takeWhileAntitone` + `fold`, line 121-131).

```haskell
builtinsIntroducedIn :: PlutusLedgerLanguage -> Map.Map MajorProtocolVersion (Set.Set DefaultFun)
builtinsIntroducedIn = \case
  PlutusV1 -> Map.fromList
    [ (alonzoPV,     Set.fromList batch1)
    , (vanRossemPV,  Set.fromList (batch2 ++ batch3 ++ batch4 ++ batch5 ++ batch6))
    ]
  PlutusV2 -> Map.fromList
    [ (vasilPV,      Set.fromList (batch1 ++ batch2))
    , (valentinePV,  Set.fromList batch3)
    , (plominPV,     Set.fromList batch4b)
    , (vanRossemPV,  Set.fromList (batch4a ++ batch5 ++ batch6))
    ]
  PlutusV3 -> Map.fromList
    [ (changPV,      Set.fromList (batch1 ++ batch2 ++ batch3 ++ batch4))
    , (plominPV,     Set.fromList batch5)
    , (vanRossemPV,  Set.fromList batch6)
    ]
```

Key surprising fact (directly answers "are batch4/5/6 builtins EVER
available to V1"): **YES** — PlutusV1 only has TWO map entries, `alonzoPV`
(batch1) and `vanRossemPV` (everything else: batch2 through batch6, all
at once). PlutusV1 is not frozen at the Alonzo base set forever; from PV11
onward a V1 script may use SerialiseData, ECDSA/Schnorr secp256k1, all
BLS12-381 ops, Keccak_256, Blake2b_224, IntegerToByteString/
ByteStringToInteger, all bitwise builtins, Ripemd_160, ExpModInteger,
DropList, the Array builtins, and the Value builtins.

## 2. Protocol version constants (`ProtocolVersions.hs`)

```
shelleyPV=2  allegraPV=3  maryPV=4  alonzoPV=5  vasilPV=7  valentinePV=8
changPV=9  plominPV=10  vanRossemPV=11
```
(PV6 = "Lobster", no name/no changes recorded; `valentinePV=8` is an
intra-Babbage HF enabling ECDSA/Schnorr secp256k1 for V2 only — not in
dugite's prior memory tables, easy to miss.) `ledgerLanguageIntroducedIn`:
PlutusV1->alonzoPV(5), PlutusV2->vasilPV(7), PlutusV3->changPV(9).

## 3. Full batch contents (also cross-validated against the Flat wire-ID
table in `plutus-flat-wire-format-defaultfun.md` — batch sizes 50/1/2/19/2/12/14
sum to exactly wire ids 0-100, confirming both independently-fetched
sources agree byte-for-byte on membership)

- **batch1** (50, wire 0-50): all Alonzo-era arithmetic/bytestring/string/
  data/list/pair builtins (AddInteger .. MkNilPairData).
- **batch2** (1, wire 51): `SerialiseData`.
- **batch3** (2, wire 52-53): `VerifyEcdsaSecp256k1Signature`,
  `VerifySchnorrSecp256k1Signature`.
- **batch4a** (19, wire 54-72): all `Bls12_381_G1_*`/`Bls12_381_G2_*`
  (add/neg/scalarMul/equal/compress/uncompress/hashToGroup),
  `Bls12_381_millerLoop`, `Bls12_381_mulMlResult`, `Bls12_381_finalVerify`,
  `Keccak_256`, `Blake2b_224`.
- **batch4b** (2, wire 73-74): `IntegerToByteString`, `ByteStringToInteger`
  — split out from batch4a specifically because V2 got these EARLIER
  (plominPV=10) than the rest of batch4 (vanRossemPV=11 for V2). Comment in
  source: enabled on V2 at PV10 via PRs #6056/#6065 but "prohibitively
  expensive" there because the cost-model param update wasn't enacted yet
  — i.e. syntactically available but economically unusable pre-enactment.
- **batch5** (12, wire 75-86): `AndByteString`, `OrByteString`,
  `XorByteString`, `ComplementByteString`, `ReadBit`, `WriteBits`,
  `ReplicateByte`, `ShiftByteString`, `RotateByteString`, `CountSetBits`,
  `FindFirstSetBit`, `Ripemd_160`.
- **batch6** (14, wire 87-100): `ExpModInteger`, `DropList`,
  `LengthOfArray`, `ListToArray`, `IndexArray`,
  `Bls12_381_G1_multiScalarMul`, `Bls12_381_G2_multiScalarMul`,
  `InsertCoin`, `LookupCoin`, `UnionValue`, `ValueContains`, `ValueData`,
  `UnValueData`, `ScaleValue`. Comment marks this batch as still OPEN
  ("Add new builtins for release in the van Rossem HF here") — future
  builtins pending a not-yet-released HF go under a NEW batch (see Note
  `[Adding new builtins: protocol versions]` in ProtocolVersions.hs:
  unreleased builtins are provisionally parked under `futurePV =
  MajorProtocolVersion maxBound` until officially enacted).

## 4. Compact builtin -> earliest (LL, PV) table

| batch | V1 avail. from | V2 avail. from | V3 avail. from |
|---|---|---|---|
| batch1 (base 50) | PV5 (alonzo) | PV7 (vasil) | PV9 (chang) |
| batch2 (SerialiseData) | PV11 (vanRossem) | PV7 (vasil, bundled w/ batch1) | PV9 (chang) |
| batch3 (ecdsa/schnorr secp256k1) | PV11 (vanRossem) | PV8 (valentine) | PV9 (chang) |
| batch4a (BLS12-381, Keccak_256, Blake2b_224) | PV11 (vanRossem) | PV11 (vanRossem) | PV9 (chang) |
| batch4b (IntegerToByteString, ByteStringToInteger) | PV11 (vanRossem) | **PV10 (plomin)** | PV9 (chang) |
| batch5 (bitwise ops, Ripemd_160) | PV11 (vanRossem) | PV11 (vanRossem) | PV10 (plomin) |
| batch6 (ExpModInteger, DropList, Array ops, Value ops) | PV11 (vanRossem) | PV11 (vanRossem) | PV11 (vanRossem) |

Answers to common gate questions:
- PlutusV2 at PV10 (plomin, mainnet-current as of dugite's preview being
  PV11): has batch1+2+3+4b — i.e. base set + SerialiseData + ECDSA/Schnorr
  secp256k1 + IntegerToByteString/ByteStringToInteger, but **NOT** BLS12-381,
  Keccak_256, Blake2b_224 (still V3-only until PV11), and **NOT** batch5/6.
- Keying is STRICTLY the `(PlutusLedgerLanguage, MajorProtocolVersion)`
  pair — `builtinsAvailableIn :: PlutusLedgerLanguage -> MajorProtocolVersion
  -> Set.Set DefaultFun`. Never PV alone, never LL alone.

## 5. How the gate is enforced at deserialisation (the rejection mechanism)

File: `plutus-ledger-api/src/PlutusLedgerApi/Common/SerialisedScript.hs`.

`scriptCBORDecoder ll pv` (line 195-239) builds THREE closures from
`builtinsAvailableIn ll pv` and `maxBoundsByPV pv` (`MaxBounds{mbHeader,
mbConstr}` — 32/1024 post-vanRossem else `maxBound`, Versions.hs:364-374)
and passes them into `UPLC.decodeProgram checkConstant checkBuiltin
checkConstr` (the flat decoder, from
`plutus-core/untyped-plutus-core/src/UntypedPlutusCore/Core/Instance/Flat.hs`):

```haskell
checkBuiltin f
  | f `Set.member` availableBuiltins = Nothing
  | otherwise = Just $ "Builtin function " ++ show f ++
      " is not available in language " ++ show (pretty ll) ++
      " at and protocol version " ++ show (pretty pv)
```

`decodeTerm`'s `handleTerm 7` (Flat.hs line 159-166) does
`case builtinPred fun of Nothing -> pure t; Just e -> fail e` — a
`MonadFail` `fail` inside Flat's own `Get` monad, evaluated WHILE decoding
the term (i.e. the very first time the offending `DefaultFun` wire-tag is
read, not after the whole program decodes).

**Critically: there is NO dedicated `ScriptDecodeError` constructor for
"builtin not available."** The failure propagates: Flat `Get` fail ->
`Flat.unflatWith` returns `Left` -> `decodeViaFlatWith` (`Codec/Extras/
SerialiseViaFlat.hs:28-32`) does `fromRightM (fail . show)`, re-raising as
a **cborg** `Decoder`-monad fail -> `CBOR.deserialiseFromBytes` returns
`Left (DeserialiseFailure byteOffset msg)` -> `deserialiseScript`'s
`toScripDecodeError` (SerialisedScript.hs:277-278) wraps it as
`CBORDeserialiseError (DeserialiseFailureInfo byteOffset (OtherReason msg))`
via `readDeserialiseFailureInfo` (`SerialiseViaFlat.hs:41-57`, which
special-cases only `"end of input"`/`"expected bytes"`; everything else,
including our builtin-availability message, falls into `OtherReason
<verbatim string>`).

So: **`ScriptDecodeError` has 4 constructors** (`CBORDeserialiseError`,
`RemainderError`, `LedgerLanguageNotAvailableError`,
`PlutusCoreLanguageNotAvailableError`) but builtin-unavailability and
constr-arity-limit-exceeded (`checkConstr n | n <= maxBoundConstr = Nothing
| otherwise = Just "constr with n fields is not available..."`, same file
line 227-234) and constant-universe-too-wide (`checkConstant`, line
207-214, gated on `mbHeader`) ALL surface as the SAME generic
`CBORDeserialiseError (DeserialiseFailureInfo _ (OtherReason msg))`
constructor — distinguishable only by string-matching the message, not by
pattern-matching a typed constructor. Only two checks get their own typed
constructor:
- `LedgerLanguageNotAvailableError{sdeAffectedLang,sdeIntroPv,sdeThisPv}` —
  checked in `deserialiseScript` itself (SerialisedScript.hs:256-259)
  BEFORE the CBOR/flat decode even starts, via
  `ledgerLanguageIntroducedIn ll <= pv`.
- `PlutusCoreLanguageNotAvailableError{sdeAffectedVersion,sdeThisLang,
  sdeThisPv}` — checked via `plcVersionsAvailableIn ll pv` but NOT in
  `deserialiseScript`/`scriptCBORDecoder` at all — it's in
  `mkTermToEvaluate` (`Common/Eval.hs:113-122`), which only runs at
  EVALUATION time. Explicit Note at Eval.hs:311-316 ("Note [Checking the
  Plutus Core language version]"): *"Since long ago this check has been in
  `mkTermToEvaluate`, which makes it a phase 2 failure. But this is really
  far too strict: we can check when deserializing, so it can be a phase 1
  failure... For now we keep it as it is."* — i.e. Plutus Core version
  (1.0.0 vs 1.1.0, which gates `constr`/`case` term syntax) availability is
  a **known-suboptimal phase-2 check**, unlike ledger-language and builtin
  availability which are phase-1.

`RemainderError` (leftover bytes after the CBOR bstr) is version-gated
separately: silently ignored for V1/V2 (legacy quirk), rejected for V3+
(SerialisedScript.hs:262-264) — unrelated to builtin availability but
shares the same `ScriptDecodeError` type.

**Phase classification**: `deserialiseScript`'s haddock (line 241-243)
states verbatim: *"Called inside phase-1 validation (i.e., deserialisation
error is a phase-1 error)."* So: ledger-language-not-available,
builtin-not-available, constr-arity-exceeded, constant-too-wide, and
RemainderError (V3+) are ALL phase-1 (script never reaches Plutus
evaluation, transaction/block is invalid). Only the PLC-version check
(constr/case v1.1.0 gate factored through `plcVersionsAvailableIn`) and of
course actual script evaluation failure are phase-2. Note this is
DIFFERENT from the `constr`/`case` wire-tag-8/9 syntax gate itself
(`unless (version >= plcVersion110) fail ...` inside `decodeTerm`,
Flat.hs:167-180), which IS inside the flat decode called from
`deserialiseScript` and so IS phase-1 — the phase-2 check in
`mkTermToEvaluate` is a redundant SECOND check (`plcVersionsAvailableIn ll
pv`, gating whether 1.1.0 syntax is allowed for this LL/PV at all, as
opposed to Flat.hs's simpler "does this program's OWN declared version
support constr/case syntactically" check). Two different axes: Flat.hs
checks the PROGRAM's declared version against a hardcoded 1.1.0 constant;
`mkTermToEvaluate` checks the PROGRAM's declared version against
`plcVersionsAvailableIn ll pv` (i.e., is 1.1.0 actually turned on for this
ledger-language+protocol-version yet).

## 6. Ledger integration point

`cardano-ledger`: `libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/
Language.hs`. Class method `decodePlutusRunnable :: Version -> Plutus l ->
Either P.ScriptDecodeError (PlutusRunnable l)` (line 422-427), per-language
instances (PlutusV1/V2/V3) all implemented as (e.g. line 474-475):
```haskell
decodePlutusRunnable pv (Plutus (PlutusBinary bs)) =
  PlutusRunnable <$> PV1.deserialiseScript (toMajorProtocolVersion pv) bs
```
i.e. directly calls the Plutus package's `deserialiseScript` (aliased per
`PlutusLedgerApi.VN`). `isValidPlutus :: PlutusLanguage l => Version ->
Plutus l -> Bool` (Language.hs:192-193) = `isRight . decodePlutusRunnable
v` — this is the closest thing to an "isScriptWellFormed" predicate; no
function of that literal name exists anywhere in IntersectMBO/plutus (grep
confirmed 2026-07-06, `1.65.0.0` tag).

## Dugite translation notes (issue #821, builtin-availability gate)

- Build the Rust gate as a static table keyed on `(PlutusLedgerLanguage,
  MajorProtocolVersion)` exactly matching section 1/4 above — do not
  collapse to a single "semantics variant" enum (that's the WRONG table;
  see `plutus-builtins-adversarial-audit.md`/`builtin-semantics-variant-
  costing.md` for the separate A-E variant axis, which governs denotation/
  costing of ALREADY-available builtins, not existence).
- Reject at flat-decode time (the moment the builtin's wire-ID is read),
  matching Haskell's per-term, first-offender ordering — not a
  whole-program pre-scan. This also matters for byte-offset-in-error
  parity if dugite ever needs to match Haskell's `DeserialiseFailureInfo`
  offset field.
- A single generic decode-rejection variant is sufficient/correct to match
  Haskell's actual granularity for builtin-unavailability, constr-arity,
  and constant-width violations (Haskell doesn't distinguish these by
  constructor either) — but ledger-language-unavailable should stay a
  distinct pre-check (run before touching the flat blob at all, matching
  `deserialiseScript`'s `llIntroPv <= pv` gate), and PLC-core-version
  (1.1.0 constr/case) has TWO distinct checks to replicate if full parity
  with the (arguably buggy/known-suboptimal) phase-1-vs-phase-2 split
  matters to dugite's Phase-1/Phase-2 error partition.
- `maxBoundsByPV` (32-byte-header / 1024-field-constr caps at PV>=11) is a
  SEPARATE, adjacent gate sharing the same `decodeProgram` call — worth
  implementing alongside the builtin gate since it's the same code path in
  Haskell and dugite issue #821 will likely want both.
