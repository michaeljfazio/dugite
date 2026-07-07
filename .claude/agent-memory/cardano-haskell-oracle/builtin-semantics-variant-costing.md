---
name: builtin-semantics-variant-costing
description: BuiltinSemanticsVariant (A-E) mapping to LL/PV, Text costing char-vs-byte/4 switch, divide/mod diagonal-shape asymmetry, cost-model-param-length handling, SatInt saturation — all source-cited from IntersectMBO/plutus master (2026-07-04 fetch)
metadata:
  type: reference
---

Full audit of the Plutus `BuiltinSemanticsVariant` mechanism as it affects
**costing** (not just denotation), fetched live from IntersectMBO/plutus
master on 2026-07-04. Answers a Dugite audit of whether applying one costing
shape unconditionally across protocol versions diverges from Haskell.

## 1. Variant count and LL/PV → variant mapping

`plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs` (~line 1075):
`data BuiltinSemanticsVariant DefaultFun = A | B | C | D | E` (5 variants,
`Enum, Bounded`).

Canonical table, `PlutusLedgerApi.Common.ProtocolVersions`
(`plutus-ledger-api/src/PlutusLedgerApi/Common/ProtocolVersions.hs`,
Note `[Mapping of protocol versions and ledger languages to semantics
variants]`, verbatim):

```
  pv pre-Conway post-Conway post-van Rossem
ll
1       A           B          D
2       A           B          D
3       C           C          E
```

PV boundaries (same file): `changPV = MajorProtocolVersion 9` (Conway/Chang),
`plominPV = 10` (Plomin, intra-era), `vanRossemPV = 11` (van Rossem,
intra-era). "post-Conway" = `9 <= pv < 11`; "post-van Rossem" = `pv >= 11`.

Implemented per-language in `PlutusLedgerApi.{V1,V2,V3}.EvaluationContext.mkEvaluationContext`,
e.g. V1/V2 (identical logic, only ParamName module differs):
```haskell
( \pv -> if | pv < changPV -> DefaultFunSemanticsVariantA
            | pv < vanRossemPV -> DefaultFunSemanticsVariantB
            | otherwise -> DefaultFunSemanticsVariantD )
```
V3:
```haskell
( \pv -> if pv < vanRossemPV then DefaultFunSemanticsVariantC else DefaultFunSemanticsVariantE )
```
So: mainnet at PV10 (Plomin, `9<=pv<11`) → V1/V2 use **B**, V3 uses **C**.
At PV11 (van Rossem) → V1/V2 use **D**, V3 uses **E**. Dugite's 5-variant
A/B/C/D/E mapping with boundaries at pv 9 and 11 is EXACTLY correct.

Separately, `ensurable semvar` (`Builtins.hs` ~line 2725) is `True` only for
D/E — gates a *denotation*-only change (bounded `CInteger` newtype wrapping
for AddInteger/SubtractInteger/etc., unrelated to costing).

## 2. Text-argument costing: char-count vs byte-length/4

`ExMemoryUsage.hs` (`plutus-core/plutus-core/src/PlutusCore/Evaluation/Machine/ExMemoryUsage.hs`):
- `instance ExMemoryUsage T.Text` (line ~325): `memoryUsage = ... T.length xs` — **character count**, chunked in a `CostRose 100` lazy spine (chunking is a laziness/interleaving detail; the total is still `T.length`).
- `newtype TextCostedByByteLength` (line ~335): `memoryUsage (TextCostedByByteLength (TI.Text _ _ lenInBytes)) = ... lenInBytes \`quot\` 4` — **UTF-8 byte length, floor-divided by 4**.

`Builtins.hs` (~1499-1579): for `AppendString`, `EqualsString`, `EncodeUtf8`
each has two denotations, `*Meaning_V1` (plain `Text`, char-count costing)
and `*Meaning_V2` (`TextCostedByByteLength` args, byte-len/4 costing), and
the variant dispatch is **identical for all three builtins**:
```
A -> _V1   B -> _V1   C -> _V1   D -> _V2   E -> _V2
```
So the switch to byte-length/4 costing happens **exactly at PV11 (van
Rossem)**, synchronized across V1, V2 (variant B→D) and V3 (variant C→E) —
not staggered per ledger-language.

Cost-model JSON params (`builtinCostModel{A,B,C,D,E}.json`) for these three
builtins are **byte-identical between the pre-vanRossem and post-vanRossem
rows** (B==C==D==E for appendString/equalsString/encodeUtf8 cpu+mem
slope/intercept/constant — only A differs, from the Conway recalibration).
I.e. the $/unit rate does NOT change at the D/E boundary; only the argument
*measure* fed into the same formula changes. Net effect: for ASCII text,
switching from char-count to byte-len/4 makes these three builtins ~4x
cheaper per character once PV11 hits, using the same rate table.

Concrete answer: `equalsString "abc" "abc"` argument size is **3** (chars) at
PV10 (variant B/C → `equalsStringMeaning_V1`, plain `Text`), and **0**
(⌊3 bytes / 4⌋) at PV11 (variant D/E → `equalsStringMeaning_V2`,
`TextCostedByByteLength`).

## 3. divideInteger/modInteger/quotientInteger/remainderInteger: NOT a uniform switch

Fetched `builtinCostModel{A,B,C,D,E}.json` cpu `"type"` for these four
builtins directly:

| builtin | A/B (pre-Conway/Conway) | C (V3, pv<11) | D (V1/V2, pv>=11) | E (V3, pv>=11) |
|---|---|---|---|---|
| divideInteger | `const_above_diagonal` | `const_above_diagonal` | **`above_and_below_diagonal`** | **`above_and_below_diagonal`** |
| modInteger | `const_above_diagonal` | `const_above_diagonal` | **`above_and_below_diagonal`** | **`above_and_below_diagonal`** |
| quotientInteger | `const_above_diagonal` | `const_above_diagonal` | `const_above_diagonal` (unchanged) | `const_above_diagonal` (unchanged) |
| remainderInteger | `const_above_diagonal` | `const_above_diagonal` | `const_above_diagonal` (unchanged) | `const_above_diagonal` (unchanged) |

**divideInteger/modInteger switch shape at PV11 for ALL ledger languages
(D and E both flip); quotientInteger/remainderInteger NEVER switch** — they
keep `const_above_diagonal` at every variant including E. This is a
builtin-specific fix, not a variant-wide shape change. A Rust impl that
applies one shared "diagonal shape" decision to all four div-family builtins
based only on PV will be wrong for two of the four.

Inner model also changes: quotientInteger/remainderInteger at D revert to
the simple `multiplied_sizes` inner model (same params as B, `intercept
228465, slope 122`) — they never picked up C's `quadratic_in_x_and_y`
inner model at all for V1/V2. Only divideInteger/modInteger's inner
`quadratic_in_x_and_y` params get recalibrated C→E (`c11: 549 -> 960`,
rest unchanged).

Exact runtime semantics, `PlutusCore.Evaluation.Machine.CostingFun.Core`
(`runTwoArgumentModel`, ~line 696-722):
```haskell
-- const_above_diagonal: cheap constant when size1 < size2, else run inner model on (size1,size2) unswapped
(ModelTwoArgumentsConstAboveDiagonal (ModelConstantOrTwoArguments c m)) =
  ... if size1 < size2 then CostLast c else run (CostLast size1) (CostLast size2)

-- above_and_below_diagonal: ALWAYS runs inner model, reordered (max,min); constant `_c` is UNUSED/dead
(ModelTwoArgumentsAboveAndBelowDiagonal (ModelConstantOrTwoArguments _c m)) =
  ... run (CostLast (max size1 size2)) (CostLast (min size1 size2))
```
So at PV10, `divideInteger` with a small dividend (size1) and huge divisor
(size2) hits the `size1 < size2` branch and is charged a **flat constant**
(`constant: 85848` in the C model). At PV11, the same call runs the full
quadratic model on `(max=divisor size, min=dividend size)` — the divisor's
size now drives a quadratic charge instead of a flat fee. This closes what
looks like an undercosting loophole (small-dividend/huge-divisor calls being
priced as if trivial) — but ONLY for divide/mod, not quot/rem, which keep
the cheap escape hatch at every PV.

## 4. Cost-model parameter array length mismatches

`PlutusLedgerApi.Common.ParamName.tagWithParamNames`
(`plutus-ledger-api/src/PlutusLedgerApi/Common/ParamName.hs` ~line 86-112):
```haskell
case lenExpected `compare` lenActual of
  EQ -> pure $ zip paramNames ledgerParams
  LT -> do  -- ledger supplied MORE than expected
    tell [CMTooManyParamsWarn {..}]
    pure $ zip paramNames ledgerParams   -- zip truncates extras, WARN only
  GT -> do  -- ledger supplied FEWER than expected
    tell [CMTooFewParamsWarn {..}]
    pure $ zip paramNames (ledgerParams ++ repeat maxBound)  -- pad tail with maxBound::Int64, WARN only
```
Both cases are **non-fatal warnings** (`CostModelApplyWarn`, via
`MonadWriter`) — evaluation context construction SUCCEEDS either way. Too-few
pads the missing tail with `maxBound::Int64` (9223372036854775807),
making any builtin whose cost params fall in that padded tail effectively
unaffordable (any real transaction budget is far below `maxBound`). Too-many
silently truncates the extra values.

Crucially, `cardano-ledger`'s `mkCostModel` (`libs/cardano-ledger-core/src/Cardano/Ledger/Plutus/CostModels.hs`
~line 276-289) calls `runWriterT (mkEvaluationContext cm)` and **discards the
warning list**, keeping only `Right (evalCtx, _)` — so length mismatches
never surface as ledger-level errors or rejections; they're silently
absorbed exactly as Plutus intends. See Note `[Cost model parameters from the
ledger's point of view]` in
`plutus-core/plutus-core/src/PlutusCore/Evaluation/Machine/CostModelInterface.hs`
(~line 89-220) for the full old-node/new-node × before/after-HF × shorter/exact/longer
compatibility matrix and rationale — this is the canonical design doc for
why the ledger can add new builtins/cost-params across a HF without breaking
old nodes.

**Entirely ABSENT cost model for an in-use language is different: it's a
Phase-1 REJECTION, not a warning.** `Cardano.Ledger.Alonzo.Plutus.Evaluate.scriptsWithContextFromLedgerTxInfoWithResult`
(`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Plutus/Evaluate.hs` line 153):
```haskell
costModel <- maybe (Left (NoCostModel lang)) Right $ Map.lookup lang $ costModelsValid costModels
```
`NoCostModel lang` is a `CollectError`, and in
`Cardano.Ledger.Alonzo.Rules.Utxos` (`eras/alonzo/impl/src/Cardano/Ledger/Alonzo/Rules/Utxos.hs`
line ~177-187) only `BadTranslation` is filtered out of the `CollectError`
list before checking non-emptiness — `NoCostModel` is NOT filtered, so it
propagates to `AlonzoUtxosPredFailure`'s `CollectErrors (NonEmpty
(CollectError era))` constructor (CBOR sum tag **1** within
`AlonzoUtxosPredFailure`; tag 0 = `ValidationTagMismatch`, tag 2 =
`UpdateFailure`). This is a genuine Phase-1 predicate failure — the whole
transaction is invalid and the block containing it (if a BP tried) would be
rejected; the tx never reaches Plutus script evaluation at all.

(Separately, `NoCostModelInLedgerState` in the same file, ~line 238/369, is a
`TransactionScriptFailure` used only by the CLI/tooling `evalTxExUnits*`
functions for local ex-units estimation — not the STS validation path.)

## 5. CostingInteger / SatInt saturation

`type CostingInteger = SatInt` (`plutus-core/plutus-core/src/PlutusCore/Evaluation/Machine/ExMemory.hs`
line 72). `ExMemory`/`ExCPU` are `newtype` wrappers deriving `Num` via
`SatInt`.

`Data.SatInt` (`plutus-core/satint/src/Data/SatInt.hs`): `newtype SatInt = SI
{unSatInt :: Int64}` — **note this is Int64-backed on all platforms** via a
64-bit unboxed-primop fast path (`addIntC#`/`subIntC#`/`timesInt2#`) plus a
portable 32-bit fallback; the `Note [Integer types for costing]` comment in
`ExMemory.hs` claiming SatInt is "backed by an Int... platform-dependent
size" is **stale relative to the current implementation** — worth flagging
but doesn't change behavior on 64-bit targets (which is all that matters;
32-bit isn't supported).

`(+)`, `(-)`, `(*)` all saturate to `maxBound`/`minBound :: Int64`
(`9223372036854775807` / `-9223372036854775808`) on overflow — confirmed via
explicit overflow-flag branches in `plusSI`/`minusSI`/`timesSI`. `negate`
special-cases `minBound -> maxBound` (since `negate minBound` would itself
overflow). `fromInteger` clamps out-of-range `Integer`s to `maxBound`/
`minBound` rather than wrapping.

Budget subtraction: `UntypedPlutusCore.Evaluation.Machine.Cek.ExBudgetMode.restricting`
(`plutus-core/untyped-plutus-core/src/UntypedPlutusCore/Evaluation/Machine/Cek/ExBudgetMode.hs`
line ~145-165):
```haskell
let cpuLeft' = cpuLeft - cpuToSpend   -- ExCPU's Num instance = SatInt saturating (-)
let memLeft' = memLeft - memToSpend
...
when (cpuLeft' < 0 || memLeft' < 0) $ throwError (CekOutOfExError ...)
```
Confirms the whole chain — additions/multiplications inside cost-model
formula evaluation, and the final remaining-budget subtraction/comparison —
is saturating `SatInt` arithmetic end-to-end, with `< 0` (post-saturation)
as the sole out-of-budget test. This is deliberate (see
`Note [Integer types for costing]`): as long as the real budget is below
`maxBound`, saturating vs true overflow give the same truth value for `a op
b < budget`, so there's no need for wrapping detection or exceptions.

## Dugite translation notes

- `crates/dugite-uplc` needs the Text-costing wrapper (byte-len/4, floor)
  gated per-call on the SAME semantics-variant enum already used for
  denotation choices — confirm it's driven by `(ledger_language,
  protocol_major_version)` exactly per the table above, not a single global
  flag, and that AppendString/EqualsString/EncodeUtf8 all flip together at
  PV11 (not per-language).
- The divideInteger/modInteger vs quotientInteger/remainderInteger diagonal-
  shape asymmetry is the highest-risk item for silent byte-exact drift if
  Dugite has ever unified "the four integer-division builtins" into one code
  path keyed only on PV — verify against `crates/dugite-uplc`'s cost-model
  dispatch for these four names specifically.
- Cost-model-param-length handling (`tagWithParamNames`) should map to:
  short array -> pad tail with i64::MAX + continue (no error); long array ->
  truncate + continue; language entirely absent from `costModels` for an
  in-use script -> reject the tx at Phase-1 (equivalent of `CollectErrors`/
  `NoCostModel`), not a runtime panic or silent fallback.
- Confirm Dugite's ExBudget/ExMemory/ExCPU analog saturates on all of
  add/sub/mul (including the `negate minBound` edge case) rather than
  panicking or wrapping on overflow, matching `SatInt`.
