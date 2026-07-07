---
name: bounded-ratio-decode-bounds-and-enact-totality
description: BoundedRatio/UnitInterval/NonNegativeInterval CBOR decode-time bound enforcement, d-param removal timeline, and Conway ENACT's totality (applyPPUpdates cannot fail)
type: reference
---

Verified live against IntersectMBO/cardano-ledger master @ `3448adc634eac8f97ec6616dc86a6c96dedab504` (2026-07-06).

## BoundedRatio / UnitInterval / NonNegativeInterval (decode-time bounds)

File: `libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes.hs`

`newtype BoundedRatio b a = BoundedRatio (Ratio a)` (line ~300). Both the modern
`DecCBOR` path (CBOR version >= 12) and the legacy `FromCBOR` path call
`boundRational` and REJECT out-of-range values — it is structurally impossible
to decode an out-of-bounds `UnitInterval`/`NonNegativeInterval`/`PositiveInterval`/
`PositiveUnitInterval` from wire bytes:

```haskell
instance (...) => DecCBOR (BoundedRatio b a) where
  decCBOR =
    ifDecoderVersionAtLeast
      (natVersion @12)
      ( do
          r <- decodeIntegralRational @a
          case boundRational r of
            Nothing -> cborError $ DecoderErrorCustom "BoundedRatio" (Text.pack $ show r)
            Just u -> pure u
      )
      (fromPlainDecoder fromCBOR)   -- legacy FromCBOR path has the SAME boundRational check
```

`decodeIntegralRational` lives in
`libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/DecCBOR.hs` (~line 214):
reads CBOR tag 30, a 2-element list `[n, d]`, fails "Denominator cannot be zero" if
`d == 0`, otherwise returns unbounded `n % d`. The *range* check (numerator <=
denominator for `UnitInterval`, i.e. ratio in [0,1]) happens one level up in
`boundRational`/`fromRatioBoundedRatio`, which compares against the type's
`Bounded` instance:

- `UnitInterval`: `minBound = 0%1, maxBound = 1%1` → decode of ratio > 1 (e.g.
  numerator > denominator) fails with `DecoderErrorCustom "BoundedRatio"`.
- `NonNegativeInterval`: `minBound = 0%1, maxBound = maxBound(Word64)%1` — i.e.
  ONLY numerator >= 0 (trivial, Word64 has no negatives) is enforced; there is
  **no finite upper bound**. `a0`/`minFeeRefScriptCostPerByte` can decode to an
  arbitrarily large ratio (up to Word64::MAX in both n and d) with no rejection.
- `PositiveInterval`/`PositiveUnitInterval` additionally reject `r == 0` via a
  nonzero `minBound` (`positiveIntervalEpsilon = 1/10^19`).

So: FACT 1 confirmed decode-REJECTS out-of-[0,1]-range `UnitInterval`; FACT 2
confirmed `NonNegativeInterval` has NO decode-time or type-level upper bound,
only a `>= 0` floor.

## Decentralisation param (`d`) removal timeline

- Shelley `PParams` (`eras/shelley/impl/src/Cardano/Ledger/Shelley/PParams.hs`):
  `sppD :: !(HKD f UnitInterval)` — present, UnitInterval-typed.
- Babbage `PParams` (`eras/babbage/impl/src/Cardano/Ledger/Babbage/PParams.hs`):
  `data BabbagePParams` has **no `d` field at all** (full field list has no
  `bppD`). `d` survives only in a separate helper record
  `data DowngradeBabbagePParams f = DowngradeBabbagePParams { dbppD :: !(HKD f
  UnitInterval), dbppExtraEntropy :: !(HKD f Nonce) }` used purely to supply a
  fixed constant when *downgrading* Babbage PParams back to Alonzo's
  PParams shape (Alonzo still has the field). So: `d` was removed as a live
  settable PParams field starting exactly at **Babbage** (not Conway), and
  Conway's `ConwayPParams` (`eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs`,
  full field list at lines 642-709) also has no `d`. Every place `d` still
  exists in the codebase, it is `UnitInterval`-typed and thus decode-bound-checked
  per the section above. `rho`/`tau` in Conway are `cppRho`/`cppTau ::
  UnitInterval` (lines 664, 666); `a0` is `cppA0 :: NonNegativeInterval` (line 662).

## Conway ENACT totality

File: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Enact.hs`.

`type PredicateFailure (ENACT era) = Void` (line 75) — the predicate-failure
type is uninhabited, so the STS rule is statically incapable of failing.
`enactmentTransition` (lines 83-116) is `pure $! case act of ...` for every
`GovAction` constructor, unconditionally. For `ParameterChange`:

```haskell
ParameterChange _ ppup _ ->
  st
    & ensCurPParamsL %~ (`applyPPUpdates` ppup)
    & ensPrevPParamUpdateL .~ SJust (GovPurposeId govActionId)
```

`applyPPUpdates :: PParams era -> PParamsUpdate era -> PParams era` (class
method, `libs/cardano-ledger-core/src/Cardano/Ledger/Core/PParams.hs` ~line
379) is TOTAL — default implementation `genericApplyPPUpdates` walks the
GHC.Generics `Rep` of both records field-by-field via a closed `Updatable`
class whose only two base instances are:

```haskell
instance Updatable (K1 t x a) (K1 t (StrictMaybe x) u) where
  applyUpdate (K1 x') (K1 sm) = K1 $ case sm of
    SJust x -> x
    SNothing -> x'
```

i.e. "if the update sets this field, overwrite; else keep old value" — no
`Either`/`Maybe`/`fail`, no re-validation of bounds, no re-check of
`ppuWellFormed`. Confirms ENACT re-validates NOTHING at enactment time.

The one-time validation happens earlier, at proposal-submission time, in the
GOV rule: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs`,
`actionWellFormed` (line ~393-399):

```haskell
actionWellFormed pv ga = failureUnless isWellFormed $ MalformedProposal ga
  where
    isWellFormed = case ga of
      ParameterChange _ ppd _ -> ppuWellFormed pv ppd
      _ -> True
```

`ppuWellFormed` (Conway instance in `Conway/PParams.hs` ~line 934) checks
semantic non-zero-ness constraints (e.g. `maxBBSize /= 0`, deposits /= 0) on
whichever fields are `SJust` in the `PParamsUpdate` — it does NOT re-check
interval bounds (those are unconditionally guaranteed by the `UnitInterval`/
`NonNegativeInterval` type itself, which can only be constructed via the
decode-time-checked `boundRational` path or by calling `boundRational`
directly in-process — there is no unsafe constructor exposed).

## Practical implication for a Rust port

If a Rust port decodes/deserializes `UnitInterval`-shaped PParams fields (rho,
tau, all voting thresholds) WITHOUT enforcing numerator<=denominator at
deserialize time, it is not just "less strict" — it's a structural divergence
from Haskell, since Haskell makes it type-impossible to hold a live
`UnitInterval > 1` anywhere past decode. Any downstream "clamp" or "ignore
out-of-range" workaround at apply/ENACT time is provably NOT what Haskell
does, because Haskell never reaches ENACT with an out-of-range value in the
first place — the CBOR decode itself is the (only) gate. Conversely, `a0` /
`NonNegativeInterval` fields legitimately have no upper bound in Haskell — a
Rust port must not impose one it invented.
