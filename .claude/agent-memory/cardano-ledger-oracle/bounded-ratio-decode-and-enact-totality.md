---
name: bounded-ratio-decode-and-enact-totality
description: Live-GitHub-verified (2026-07-06) — UnitInterval CBOR decode rejects out-of-[0,1] ratios at decode time, NonNegativeInterval has no upper bound, Conway ENACT applyPPUpdates is total (PredicateFailure = Void)
metadata:
  type: reference
---

Verified against IntersectMBO/cardano-ledger master @ `3448adc634eac8f97ec6616dc86a6c96dedab504` (2026-07-06, via cardano-haskell-oracle live fetch). Answers issue #802 (dugite protocol-param enactment bugfix gate).

## 1. `UnitInterval` (`BoundedRatio b a`) decode-time bound enforcement

`libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes.hs`:
```haskell
newtype BoundedRatio b a = BoundedRatio (Ratio a)   -- line ~300

instance (...) => DecCBOR (BoundedRatio b a) where   -- line ~412-433
  decCBOR =
    ifDecoderVersionAtLeast (natVersion @12)
      ( do
          r <- decodeIntegralRational @a
          case boundRational r of
            Nothing -> cborError $ DecoderErrorCustom "BoundedRatio" (Text.pack $ show r)
            Just u -> pure u
      )
      (fromPlainDecoder fromCBOR)   -- legacy FromCBOR path has the SAME boundRational check

instance Integral a => Bounded (BoundedRatio UnitInterval a) where   -- line ~564-566
  minBound = BoundedRatio (0 % 1)
  maxBound = BoundedRatio (1 % 1)
```
It is structurally IMPOSSIBLE to decode an out-of-[0,1] `UnitInterval` from CBOR wire bytes — both modern (PV≥12 direct-rational tag-30 path) and legacy decode paths route through `boundRational`, which fails the whole decoder (`DecoderErrorCustom "BoundedRatio"`) if numerator/denominator produce a ratio outside `[minBound,maxBound]`. No unsafe/unchecked constructor for `UnitInterval` is exposed anywhere in the codebase.

`rho` (monetary expansion) and `tau` (treasury cut) are `UnitInterval`-typed in Conway (`eras/conway/impl/src/Cardano/Ledger/Conway/PParams.hs:664,666`, `cppRho`/`cppTau`). `d` (decentralisation) was **removed from PParams at Babbage** (Shelley's `PParams` still has `sppD :: UnitInterval`, `eras/shelley/impl/.../PParams.hs:123`; Babbage's live record has no `d` field — it survives only in `DowngradeBabbagePParams` as a fixed downgrade constant, `eras/babbage/impl/.../PParams.hs:167-170`). Conway has no `d` field either. Everywhere `d` still exists it is `UnitInterval`-typed, so same decode-time gate applies.

## 2. `NonNegativeInterval` — floor at 0, NO finite upper bound

```haskell
newtype NonNegativeInterval = NonNegativeInterval (BoundedRatio NonNegativeInterval Word64)  -- line ~474-487

instance Bounded (BoundedRatio NonNegativeInterval Word64) where   -- line ~489-491
  minBound = BoundedRatio (0 % 1)
  maxBound = BoundedRatio (maxBound % 1)   -- Word64::MAX % 1
```
Only `numerator >= 0` and staying within `Word64` range are enforced (via the same `boundRational` overflow guard) — no semantic upper bound. `a0` (pool pledge influence, `cppA0`, PParams.hs:662) and `minFeeRefScriptCostPerByte` (line 706-707) are both `NonNegativeInterval`-typed and can be arbitrarily large by design. **Do not invent an upper-bound clamp for `a0` in a Rust port — Haskell has none.**

## 3. Conway ENACT is total — `PredicateFailure (ENACT era) = Void`

`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Enact.hs`:
```haskell
instance EraGov era => STS (ENACT era) where
  type PredicateFailure (ENACT era) = Void          -- line ~75

enactmentTransition = do
  TRC ((), st, EnactSignal govActionId act) <- judgmentContext
  pure $! case act of
    ParameterChange _ ppup _ ->
      st & ensCurPParamsL %~ (`applyPPUpdates` ppup)
         & ensPrevPParamUpdateL .~ SJust (GovPurposeId govActionId)
    ...   -- every branch (NoConfidence/UpdateCommittee/NewConstitution/HardForkInitiation/TreasuryWithdrawals/InfoAction) is a pure unconditional update, lines ~83-116
```
`applyPPUpdates :: PParams era -> PParamsUpdate era -> PParams era` (`libs/cardano-ledger-core/src/Cardano/Ledger/Core/PParams.hs:379-392`, default = `genericApplyPPUpdates`) walks GHC.Generics field-by-field via `Updatable`, base case:
```haskell
instance Updatable (K1 t x a) (K1 t (StrictMaybe x) u) where
  applyUpdate (K1 x') (K1 sm) = K1 $ case sm of
    SJust x -> x
    SNothing -> x'
```
Total — no `Either`/`Maybe`/`fail`, no bound re-check, no `ppuWellFormed` re-check at enactment. The ONE-TIME validation is earlier, at proposal submission in GOV (`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Gov.hs:393-399`, `actionWellFormed` → `ppuWellFormed` → `MalformedProposal` on failure). `ppuWellFormed` (Conway instance, `Conway/PParams.hs:~934`) checks only semantic non-zero constraints on `SJust`-set fields — it does NOT re-check interval bounds, because those are already type-guaranteed unrepresentable-if-invalid by the decode-time gate in fact 1/2 above.

## 4. `Rational`/`BoundedRatio` decode ALWAYS reduces numerator/denominator to lowest terms — verified live 2026-07-06

Every decode path for a `Rational` or `BoundedRatio` (`UnitInterval`, `NonNegativeInterval`, `PositiveInterval`) constructs the value via GHC's `%` smart constructor (`Data.Ratio.%` from `base`), which is defined as `x % y = reduce (x * signum y) (abs y)` where `reduce` divides both sides by `gcd x y` — this is a `base`-library invariant with no public unsafe/raw `:%` constructor exposed anywhere in cardano-ledger. Concretely:

- `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Decoding/DecCBOR.hs`:
  ```haskell
  decodeIntegralRational :: forall a s. (DecCBOR a, Integral a) => Decoder s Rational
  decodeIntegralRational = do
    assertTag 30
    values <- decodeList (decCBOR @a)
    case values of
      [n, d] -> do
        when (d == 0) $ fail "Denominator cannot be zero"
        pure $! toInteger n % toInteger d          -- <-- reduces here
      xs -> cborError $ DecoderErrorSizeMismatch "Rational" 2 (length xs)
  ```
  This is what `BoundedRatio`'s modern (PV≥12) `DecCBOR` path calls (fact 1 above, `decCBOR = ifDecoderVersionAtLeast (natVersion @12) (... decodeIntegralRational @a ...) (...)`).
- The legacy pre-PV12 `FromCBOR (BoundedRatio b a)` path (`libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes.hs:392-408`) calls `Plain.decodeRationalWithTag`, which bottoms out in `libs/cardano-ledger-binary/src/.../Decoder.hs::decodeRationalWithoutTag`: `pure $! n % d` — same reduction.
- The plain (non-bounded) `Rational` decoder used elsewhere (`decodeRational`/`decodeRationalFixedSizeTuple` in the same `Decoder.hs`) also constructs via `n % d` / `return $! n % d` in every version branch (PV1 fixed-tuple, PV≥2 variable-length, PV≥9 tag-30-optional).

**Consequence**: a non-canonical on-wire encoding such as `[9, 18]` for a `UnitInterval`/`NonNegativeInterval`/generic `Rational` field decodes to `9 % 18` which GHC's `%` immediately normalizes to `1 % 2` — by the time the value reaches `ToPlutusData` (`toPlutusData = toPlutusData . unboundRational` → `Rational` → `List [I num, I den]`, see [[changed-parameters-plutus-data-encoding]]), it is **already reduced**; there is no code path in cardano-ledger that can carry an unreduced numerator/denominator pair from CBOR wire bytes through to a `Data::List` in `ChangedParameters`/`ScriptContext`. It never fails to decode either — reduction happens silently and unconditionally, not as a validation gate.

**dugite finding (CONFIRMED DIVERGENCE)**: `crates/dugite-serialization/src/decode/reader.rs::read_rational` decodes `numerator`/`denominator` as raw `u64`s straight off the wire with **no gcd-reduction step**, and `dugite_primitives::transaction::Rational { numerator, denominator }` (`crates/dugite-primitives/src/transaction.rs:352`) has no normalizing constructor either — confirmed by grep, every call site (including `crates/dugite-uplc/src/populate_gov.rs::rat()`, used for `a0`/`rho`/`tau`/`minFeeRefScriptCostPerByte`/voting-threshold rationals in `ChangedParameters`) emits the struct's stored fields verbatim into `Data::List [I num, I den]`. A non-canonical (but validly-decodable) on-chain `PParamsUpdate` with an unreduced rational would therefore produce a byte-different `ChangedParameters` Data blob vs. Haskell for any V3 script inspecting a `ParameterChange` governance action in its `ScriptContext`. Threshold *comparisons* (`Rational::is_met_by`, cross-multiplication) are reduction-invariant and unaffected — this only matters for values that get **serialized into Plutus Data** or otherwise byte-compared. Fix = reduce by `gcd(numerator, denominator)` (and canonicalize sign, though these fields are non-negative by ledger typing) either at `read_rational` decode time (closest to matching Haskell's decode-time reduction) or immediately before Plutus Data emission.

## Practical implication for dugite-ledger

If dugite deserializes/applies `UnitInterval`-typed PParams fields (rho, tau, pool margin, thresholds) without enforcing `numerator <= denominator` at the point of untrusted-bytes ingestion (CBOR decode of a tx-borne `PParamsUpdate`, or governance genesis config), that is a real structural divergence from Haskell, which makes an out-of-range `UnitInterval` unrepresentable past decode — so Haskell's ENACT never needs to guard against it. Any clamp-at-apply-time or silent-ignore logic in the Rust port is provably not what Haskell does; the correct fix is to reject at decode/proposal-parse time (mirroring `MalformedProposal`/`DecoderErrorCustom "BoundedRatio"`), not at enactment.

## Related
[[oracle_ledger_governance]] (ENACT rule summary, priority order) — this file adds the exact totality proof.
[[conway-pparams-field-order]] — field types for rho/tau/a0/minFeeRefScriptCostPerByte.
[[conway-ratify-precision-facts]] — sibling live-verified RATIFY/GOV precision pass from 2026-07-04; this file is the Enact/BaseTypes complement for issue #802.
[[plutus-txinfo-translation-v3unit-byron-pointer]] — sibling live-verified pass (2026-07-06) covering the other 3 questions of the same audit (V3 unit-return check, Byron address Alonzo-drop/Babbage-error, StakingPtr passthrough).
[[project_dugite_plutus_context_audit_2026_07_06]] — audit ticket tracking the dugite-side divergence found in fact 4 above.
Sister write-up in the other oracle's memory: `.claude/agent-memory/cardano-haskell-oracle/bounded-ratio-decode-bounds-and-enact-totality.md`.
