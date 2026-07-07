---
name: v3-paramname-vanrossem-tail-297-349
description: Exact V3 ParamName declaration order + cost-function shape for indices 297-349 (van Rossem/PV11/batch6 tail, 53 fields across 14 builtins), cross-validated against source, builtinCostModelE.json, and live preview PV11 protocol params (V3 total=350).
metadata:
  type: reference
---

Verified 2026-07-06. Sources: `plutus-ledger-api/src/PlutusLedgerApi/V3/
ParamName.hs` (master), `plutus-core/plutus-core/src/PlutusCore/Evaluation/
Machine/BuiltinCostModel.hs` (field arity per builtin, `paramX :: f ModelN
Arguments`), `.../CostingFun/Core.hs` (shape constructors + evaluators,
`ModelOneArgument`/`ModelTwoArguments`/`ModelThreeArguments`/
`ModelFourArguments` sum types), `plutus-core/cost-model/data/
builtinCostModelE.json` (concrete `"type"` tag per builtin, confirms shape
choice), and live preview `cli_protocol_params` (`PlutusV3` array length =
350, matches source count exactly: total V3 `ParamName` constructors =
350, confirmed via clean extraction of the `data ParamName = ... deriving
stock` block, 0-based indices 0-349).

Batch6 = "to be deployed in PV11" / van Rossem, source comment at
`ParamName.hs` line ~316: `-- To be deployed in PV11`. Indices below are
0-based positions in the full V3 `ParamName` enum (this is also the
declaration order `tagWithParamNames`'s `zip` walks — see
`PlutusLedgerApi/Common/ParamName.hs`).

## Table: index, constructor, shape, field count

| idx | ParamName constructor | builtin | cpu shape | cpu fields | mem shape | mem fields |
|---|---|---|---|---|---|---|
| 297-301 | `ExpModInteger'cpu'arguments'coefficient00/11/12`, `'memory'arguments'intercept/slope` | expModInteger | `exp_mod_cost` (custom `ExpModCostingFunction`) | 3 (`coefficient00,coefficient11,coefficient12`) | `linear_in_z` | 2 (`intercept,slope`) |
| 302-304 | `DropList'cpu'arguments'intercept/slope`, `'memory'arguments` | dropList | `linear_in_x` | 2 | `constant_cost` | 1 |
| 305-306 | `LengthOfArray'cpu'arguments`, `'memory'arguments` | lengthOfArray | `constant_cost` | 1 | `constant_cost` | 1 |
| 307-310 | `ListToArray'cpu'arguments'intercept/slope`, `'memory'arguments'intercept/slope` | listToArray | `linear_in_x` | 2 | `linear_in_x` | 2 |
| 311-312 | `IndexArray'cpu'arguments`, `'memory'arguments` | indexArray | `constant_cost` | 1 | `constant_cost` | 1 |
| 313-315 | `Bls12_381_G1_multiScalarMul'cpu'arguments'intercept/slope`, `'memory'arguments` | bls12_381_G1_multiScalarMul | `linear_in_x` | 2 | `constant_cost` | 1 |
| 316-318 | `Bls12_381_G2_multiScalarMul'cpu'arguments'intercept/slope`, `'memory'arguments` | bls12_381_G2_multiScalarMul | `linear_in_x` | 2 | `constant_cost` | 1 |
| 319-322 | `InsertCoin'cpu'arguments'intercept/slope`, `'memory'arguments'intercept/slope` | insertCoin | `linear_in_u` | 2 | `linear_in_u` | 2 |
| 323-325 | `LookupCoin'cpu'arguments'intercept/slope`, `'memory'arguments` | lookupCoin | `linear_in_z` | 2 | `constant_cost` | 1 |
| 326-331 | `UnionValue'cpu'arguments'c00/c10/c01/c11`, `'memory'arguments'intercept/slope` | unionValue | `with_interaction_in_x_and_y` | 4 (`c00,c10,c01,c11` — record field order, NOT alphabetical) | `added_sizes` | 2 |
| 332-336 | `ValueContains'cpu'arguments'constant`, `'model'arguments'intercept/slope1/slope2`, `'memory'arguments` | valueContains | `const_above_diagonal` (wraps `linear_in_x_and_y`) | 4 (`constant` + `intercept,slope1,slope2`) | `constant_cost` | 1 |
| 337-340 | `ValueData'cpu'arguments'intercept/slope`, `'memory'arguments'intercept/slope` | valueData | `linear_in_x` | 2 | `linear_in_x` | 2 |
| 341-345 | `UnValueData'cpu'arguments'c0/c1/c2`, `'memory'arguments'intercept/slope` | unValueData | `quadratic_in_x` | 3 (`c0,c1,c2`) | `linear_in_x` | 2 |
| 346-349 | `ScaleValue'cpu'arguments'intercept/slope`, `'memory'arguments'intercept/slope` | scaleValue | `linear_in_y` | 2 | `linear_in_y` | 2 |

Total: 53 fields (297..349 inclusive), 14 builtins. Sum check:
5+3+2+4+2+3+3+4+3+6+5+4+5+4 = 53. V3 total constructor count = 350
(0..349), matches live preview `PlutusV3` costModels array length
exactly.

## ExpModInteger formula (answers "is it c00+c11*ee*mm+c12*ee*mm*mm")

Confirmed exact, from `CostingFun/Core.hs`:

```haskell
data ExpModCostingFunction = ExpModCostingFunction
  { coefficient00 :: Coefficient00
  , coefficient11 :: Coefficient11
  , coefficient12 :: Coefficient12
  }
-- evaluateExpModCostingFunction (ExpModCostingFunction c00 c11 c12) aa ee mm =
--   let cost0 = c00 + c11*ee*mm + c12*ee*mm*mm
--   in if aa <= mm then cost0 else cost0 + (cost0 `dividedBy` 2)
```

Field order is `coefficient00, coefficient11, coefficient12` (3 params,
NOT a generic `ModelThreeArguments` shape — it's the one-off
`ModelThreeArgumentsExpModCost ExpModCostingFunction` constructor, a
named custom model as suspected). `aa`/`ee`/`mm` are the memory-usage
sizes of `expModInteger`'s 3 args (base, exponent, modulus) in that
argument order — cost0 uses `ee` (exponent size) and `mm` (modulus size)
only, `aa` (base size) only affects the +50% penalty gate (`aa <= mm`
check), never appears in the cost0 polynomial itself. Memory cost is a
**separate, independent** `linear_in_z` model (2 fields: intercept,
slope) applied only to `mm` (the 3rd/"z" arg) — confirmed via
`ModelThreeArgumentsLinearInZ (OneVariableLinearFunction intercept slope)`
scaling `costs3` (the third argument's cost stream) only. Memory fields
come immediately after the 3 cpu coefficient fields in `ParamName`
declaration order (coefficient00, coefficient11, coefficient12,
intercept, slope) — no other field interleaved.

## Cross-check note

This same builtin set (batch6) and same shapes apply to PlutusV1 and
PlutusV2 too (both get batch6 at `vanRossemPV=11`) — see
[[v2v1-paramname-vanrossem-extension-live]] for why V1/V2 are NOT frozen
at their historical base counts, and why this is a live-on-preview-today
concern, not a future-PV-only concern. V2's batch6 slice starts at V2's
own index 279 (not 297 — V2's overall array is shorter, 332 vs 350,
due to unrelated shape differences in the DivideInteger/ModInteger/
QuotientInteger/RemainderInteger family within the original 0-174 base),
but the batch6 shapes/field-counts/formulas themselves are identical to
what's tabulated above.
