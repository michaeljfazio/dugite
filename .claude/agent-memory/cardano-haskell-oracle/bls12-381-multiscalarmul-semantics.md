---
name: bls12-381-multiscalarmul-semantics
description: bls12_381_G{1,2}_multiScalarMul denotation source (zip-truncation, empty-list identity, scalar bounds) + readKnownConstant/geqL list element-type-tag check (structural, empty-list-inclusive); dugite bls.rs currently skips the elem_type check for empty lists
type: reference
---

Source-verified 2026-07-06 against IntersectMBO/plutus master (Builtins.hs wire
IDs 92/93 confirmed against tag 1.65.0.0 per [[plutus-flat-wire-format-defaultfun]])
and IntersectMBO/cardano-base master (`blsMSM`).

## 1. Denotation source (answers "truncate vs error vs pad")

`plutus-core/plutus-core/src/PlutusCore/Crypto/BLS12_381/G1.hs` (G2.hs is
byte-identical modulo curve):
```haskell
multiScalarMul :: [Integer] -> [Element] -> BuiltinResult Element
multiScalarMul ss p
  | any msmScalarOutOfBounds ss = fail "Scalar exceeds 512-byte bound for G1.multiScalarMul"
  | otherwise = pure . coerce $ BlstBindings.blsMSM @BlstBindings.Curve1 (zip ss (coerce p))
```
**`zip ss (coerce p)`** — plain `Prelude.zip`, which stops at the shorter
list. No `length`-equality check exists anywhere in this function or in its
Builtins.hs wiring (`toBuiltinMeaning _semvar Bls12_381_G1_multiScalarMul`,
~line 2402-2417, is a direct, non-semvar-gated pass-through to this
denotation — no extra check layered on top). **Mismatched lengths => silent
truncation to `min(len ss, len p)` pairs, never an error, never padding.**

`msmScalarOutOfBounds` (`.../BLS12_381/Bounds.hs`): bound is `[-(2^4095),
2^4095 - 1]` (64 words = 512 bytes = 4096 bits, one bit for sign gives
2^4095). Out-of-range scalar (in EITHER list position, checked via `any`
over the whole `ss` list before `zip` even runs) => `BuiltinResult` failure,
regardless of whether that scalar would even survive to be zipped with a
point.

## 2. Empty x empty (`blsMSM []`)

`cardano-base/cardano-crypto-class/src/Cardano/Crypto/EllipticCurve/BLS12_381/Internal.hs`
`blsMSM` (~line 1107-1172): folds `ssAndps` filtering out (a) points at
infinity and (b) zero-scalar pairs (both silently dropped, no error —
`blst_to_affines` would itself fail on an infinity point, and zero-scalar
pairs contribute nothing), THEN:
```haskell
case filteredPoints of
  []              -> return blsZero
  [(scalar, pt)]  -> ... blsMult pt i   -- single-pair fast path
  _               -> ... c_blst_mult_pippenger ...
```
`zip [] []` = `[]` => `filteredPoints = []` => **`blsMSM` returns `blsZero`
(group identity) successfully — not an error.** This is the SAME code path
reached whenever the truncated zip ends up empty (e.g. an empty first list
paired with a non-empty second list, or vice versa), not a special case
carved out only for `[] []`.

## 3. `readKnown`/unlifting list element-type check (empty list included)

`multiScalarMul :: [Integer] -> [Element] -> BuiltinResult Element` uses
plain monomorphic Haskell list types, so each argument goes through the
generic `ReadKnownIn` default (`readKnown = inline readKnownConstant`,
`plutus-core/plutus-core/src/PlutusCore/Builtin/KnownType.hs` line 310-323):
```haskell
readKnownConstant val =
  asConstant val >>= oneShot \case
    Some (ValueOf uniAct x) -> do
      let uniExp = knownUni @_ @(UniOf val) @a
      case uniExp `geqL` uniAct of
        EvaluationSuccess Refl -> pure x
        EvaluationFailure ->
          throwError . BuiltinUnliftingEvaluationError $ typeMismatchError uniExp uniAct
```
`Some (ValueOf uniAct x)` comes straight from the term's `Constant`
constructor (`HasConstant.hs`: `asConstant (Constant _ val) = pure val`) —
i.e. `uniAct` is the type witness that was embedded in the `con` term AT
PARSE TIME (from the type annotation in `(con (list <ty>) [...])`,
independent of how many elements follow). `uniExp` for `a = [Element]` is
built by the generic `Contains` instance
(`PlutusCore/Default/Universe.hs` line 340-341/356/363-366):
`knownUni @[G1.Element] = DefaultUniProtoList \`DefaultUniApply\` DefaultUniBLS12_381_G1_Element`.

The comparison itself (`geqL`, same file, line 175-178):
```haskell
geqL (DefaultUniProtoList `DefaultUniApply` a1) listA2 = do
  DefaultUniProtoList `DefaultUniApply` a2 <- pure listA2
  Refl <- geqL (LoopBreaker a1) (LoopBreaker a2)
  pure Refl
```
This pattern-matches purely on the GADT type-tag structure (`DefaultUniProtoList`/`DefaultUniApply`
constructors and the recursive element-type witness) — **it never inspects
`x` (the actual `[Element]` value) at all**, so the check is applied
identically whether the list has 0 or N elements. **Confirmed: `(con (list
bls12_381_G2_element) [])` fed where `[G1.Element]` is expected FAILS with a
`BuiltinUnliftingEvaluationError`/type-mismatch (StructuralError), exactly
like a non-empty wrong-type list would — an empty list of the wrong declared
element type does NOT unlift successfully.**

## Conformance-vector cross-check (dugite's own upstream corpus)

`crates/dugite-uplc/tests/conformance/builtin/semantics/bls12_381_G1_multiScalarMul/`
(G2 dir is a parallel set) — comment headers per test, decisive evidence for
truncation vs error:
- `multiScalarMul-08`: `(con (list integer) [])` x `(con (list
  bls12_381_G1_element) [])` — "Both arguments are empty lists => result =
  zero", expects `True` (equal to the compressed-identity point). Literal
  `[] []` case.
- `multiScalarMul-06a`/`07`: ONE list empty, the other non-empty (e.g. empty
  scalar list vs 6 points) — "=> result = zero", expects `True`. This alone
  already rules out a strict-equal-length implementation (which would abort
  evaluation, not yield a comparable `True`).
- `multiScalarMul-09a`/`09b`/`10a`/`10b`: **the decisive vectors** — directly
  assert `msm(13-scalar-list, 6-point-list) == msm(first-6-of-that-scalar-list,
  same-6-point-list)`, comment "Extra entries at end of first/second list are
  ignored", expects `True`. Only exact `zip`-truncation semantics make this
  hold; a padding or strict-length-check implementation would not.
- `multiScalarMul-12a`: verified by direct byte-count — 11 zero scalars vs 24
  points (not "11x24 non-trivial", contra an initial guess) — mismatched
  lengths truncate to 11 pairs, all zero-scalar so every pair is filtered by
  `blsMSM`'s zero-scalar skip, net result is the identity point; consistent
  with but not independently decisive for zip-vs-strict (09a/10a are the
  decisive ones).
- `multiScalarMul-13a`-`13d`: scalar-bounds edge cases (`2^4095-1` OK,
  `2^4095` errors; `-2^4095` OK, `-2^4095-1` errors) — matches
  `msmScalarLb`/`msmScalarUb` exactly.

## Dugite gap found while cross-checking (2026-07-06)

`crates/dugite-uplc/src/builtin/bls.rs::denote_multi_scalar_mul` (~line
693-827) already implements zip-truncation correctly (explicit comment
"Truncate to the shorter list (Haskell zip semantics)", line ~756/805) and
the exact `[-(2^4095), 2^4095-1]` scalar bound (`bigint_in_msm_scalar_range`,
line ~542, matches `msmScalarLb`/`msmScalarUb` bit-for-bit).

**But it does NOT replicate the `geqL` static element-type check for empty
lists.** It inline-pattern-matches `Value::Const(Constant::ProtoList {
elements, .. })` (discarding the `elem_type` field via `..`) and validates
element type only by matching each item's `Constant` discriminant inside
`.map()` — for an empty `elements: vec![]` this loop trivially succeeds
without ever consulting `elem_type`. Contrast with `mkCons`'s handling in
the same crate (`denotations.rs` line ~421, `if head_ty != elem_type`),
which DOES check the declared `elem_type` field directly — that's the
existing in-repo pattern this should follow. Net effect: dugite currently
accepts e.g. `(con (list bls12_381_G2_element) [])` where Haskell's
`bls12_381_G1_multiScalarMul` requires `[G1.Element]` (or the mirrored G1
list fed to `G2_multiScalarMul`), silently proceeding as an empty point
list instead of raising `BuiltinUnliftingEvaluationError`. Since this is
only reachable via a hand-crafted/adversarial flat-encoded UPLC term (not
producible by normal PlutusTx compilation), it's a phase-2
accept-when-Haskell-would-reject divergence, not a wrong-numeric-result bug
— but per [[feedback_haskell_byte_exact_only]] and
[[feedback_dugite_node_hostile_environment]] this still needs a fix: check
`scalars_val`'s and `points_val`'s `elem_type` against the expected
`TypeTag::Integer` / `TypeTag::Bls12_381G1Element` (or G2) BEFORE inspecting
`elements`, unconditional on list length. Not yet filed as a GitHub issue as
of this writing.

## Key files for quick re-fetch
- `plutus-core/plutus-core/src/PlutusCore/Crypto/BLS12_381/{G1,G2}.hs` (`multiScalarMul`)
- `plutus-core/plutus-core/src/PlutusCore/Crypto/BLS12_381/Bounds.hs` (`msmScalarOutOfBounds`/Lb/Ub)
- `plutus-core/plutus-core/src/PlutusCore/Default/Builtins.hs` ~line 2402-2417 (wiring, wire IDs 92/93)
- `plutus-core/plutus-core/src/PlutusCore/Builtin/KnownType.hs` line 310-323 (`readKnownConstant`)
- `plutus-core/plutus-core/src/PlutusCore/Builtin/HasConstant.hs` line 59-61 (`asConstant`)
- `plutus-core/plutus-core/src/PlutusCore/Default/Universe.hs` line 159-205 (`geqL` instance), line 340-368 (`Contains`/`knownUni`)
- `cardano-base/cardano-crypto-class/src/Cardano/Crypto/EllipticCurve/BLS12_381/Internal.hs` line 1107-1172 (`blsMSM`)
- Dugite: `crates/dugite-uplc/src/builtin/bls.rs` (`denote_multi_scalar_mul`, ~line 693-827), `crates/dugite-uplc/src/builtin/denotations.rs` (`unwrap_proto_list` line 1602, `mkCons` elem_type check ~line 421)
