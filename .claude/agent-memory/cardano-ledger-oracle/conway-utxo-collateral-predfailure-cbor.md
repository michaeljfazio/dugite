---
name: conway-utxo-collateral-predfailure-cbor
description: Exact Sum-tag numbers + EncCBOR payload shapes for CollateralContainsNonADA (15), InsufficientCollateral (12), BabbageNonDisjointRefInputs (22) in ConwayUtxoPredFailure; DeltaCoin and NonEmpty EncCBOR mechanics; CollateralContainsNonADA's TRIGGER is netted (inputs minus return) even though its error payload sometimes isn't.
metadata:
  type: reference
---

Live-verified 2026-08-06 @ cardano-ledger commit `f8d6ead7c84e87b175efe3259fa838210be6c2d2`
(master, 2026-08-05). Source: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Utxo.hs`
(the `ConwayUtxoPredFailure` sum type, encoder/decoder ~lines 72-368) and
`eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Utxo.hs` +
`.../Babbage/Collateral.hs` (raise sites).

## The three tags (Conway's own Sum type, NOT inherited byte-for-byte from Babbage/Alonzo)

`ConwayUtxoPredFailure` is a standalone 23-constructor sum (tags 0-22) with its OWN
`Sum`/`SumD` encode/decode table — `babbageToConwayUtxoPredFailure` /
`alonzoToConwayUtxoPredFailure` translate constructors from earlier eras' predicate-failure
types into this one when injecting a failure, they do not reuse Babbage's tag numbers.

```haskell
InsufficientCollateral a b -> Sum InsufficientCollateral 12 !> To a !> To b   -- tag 12
CollateralContainsNonADA a -> Sum CollateralContainsNonADA 15 !> To a         -- tag 15
BabbageNonDisjointRefInputs x -> Sum BabbageNonDisjointRefInputs 22 !> To x   -- tag 22
```
Decoder table confirms field order: `12 -> SumD InsufficientCollateral <! From <! From`
(DeltaCoin decoded first, then Coin — matches constructor declaration order, not swapped).

Full constructor declarations (`ConwayUtxoPredFailure`, lines ~72-144):
```haskell
| InsufficientCollateral
    DeltaCoin   -- balance computed
    Coin        -- the required collateral for the given fee
...
| CollateralContainsNonADA (Value era)
...
| BabbageNonDisjointRefInputs (NonEmpty TxIn)
```
`Value ConwayEra = MaryValue` (`eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs`), so
`CollateralContainsNonADA` carries a FULL `MaryValue` (ada + multi-asset map), not a
Coin-only or assets-only projection.

## Q1 — CollateralContainsNonADA: the TRIGGER is netted; only the error PAYLOAD is not

**Correction (2026-08-06, same session, caught by a tech-lead peer-review):** an earlier
draft of this note quoted only the payload-selection logic (`valueWithNonAda`) and wrongly
extrapolated its "never netted" comment to the firing condition itself. They are two
separate pieces of logic — the trigger IS netted. Full function, `validateCollateralContainsNonADA`
(Babbage Utxo.hs ~253-292, unchanged in Conway):

```haskell
validateCollateralContainsNonADA txBody utxoCollateral =
  failureUnless onlyAdaInCollateral $ Alonzo.CollateralContainsNonADA valueWithNonAda
  where
    onlyAdaInCollateral =
      utxoCollateralAndReturnHaveOnlyAda || allNonAdaIsConsumedByReturn
    -- fast-path short-circuit: skip the full Value computation when both sides are
    -- already trivially ada-only
    utxoCollateralAndReturnHaveOnlyAda =
      utxoCollateralHasOnlyAda && areAllAdaOnly (txBody ^. collateralReturnTxBodyL)
    utxoCollateralHasOnlyAda = areAllAdaOnly utxoCollateral
    -- THE ACTUAL (general) TRIGGER: net balance (inputs minus return, full Value
    -- subtraction) has an empty MultiAsset map
    allNonAdaIsConsumedByReturn = Val.isAdaOnly totalCollateralBalance
    valueWithNonAda =
      case txBody ^. collateralReturnTxBodyL of
        SNothing -> collateralBalance
        SJust retTxOut ->
          if utxoCollateralHasOnlyAda
            then retTxOut ^. valueTxOutL
            else collateralBalance
    collateralBalance = sumAllValue utxoCollateral            -- raw sum of collateral INPUT TxOuts' Value (Map.restrictKeys utxo collateralInputsTxBodyL upstream — inputs only, resolved)
    totalCollateralBalance = case txBody ^. collateralReturnTxBodyL of
      SNothing -> collateralBalance
      SJust retTxOut -> collateralBalance <-> (retTxOut ^. valueTxOutL @era)   -- NETTED
```
So the predicate PASSES (no failure) iff EITHER both sides are already ada-only (pure
optimization, semantically redundant with the second clause) OR the net Value
`collateralBalance <-> return.value` is ada-only. **A wallet spending a 100% multi-asset
UTxO as collateral and returning ALL of it as change via `collateral_return` does NOT
trigger this failure** — confirms the intuitive "collateral_return exists so tokens can be
given back" reading is correct.

Netting mechanics that make "fully consumed" work: `<->` is `Val`'s default
`x <+> invert y` (`libs/cardano-ledger-core/src/Cardano/Ledger/Val.hs`); `MaryValue`'s
`Semigroup`/`Group` delegate to `MultiAsset`'s `canonicalMapUnion`/`canonicalMap`
(`libs/cardano-data/src/Data/CanonicalMaps.hs`), which **prune any map entry whose value
becomes exactly zero** after combining ("a `CanonicalZero` map never stores a zero"). So an
asset returned in the exact quantity it was consumed as collateral cancels its map entry
to nothing, not to a zero-valued entry — `Map.null` (what `isAdaOnly (MaryValue _ (MultiAsset m)) = Map.null m` checks, `eras/mary/impl/src/Cardano/Ledger/Mary/Value.hs`) then
sees a genuinely empty map. Any residual (over-return, under-return, or an asset present on
one side only) leaves a nonzero entry and the check fails.

`utxoCollateral` (feeding `collateralBalance`) is confirmed inputs-only: the call site in
`validateTotalCollateral`/`feesOK` builds it as
`utxoCollateral = Map.restrictKeys utxo (txBody ^. collateralInputsTxBodyL)` — i.e. the
resolved TxOuts of the collateral INPUT set, never touching `collateralReturnTxBodyL`
except in the explicit `<->` netting step shown above.

`areAllAdaOnly = all (^. isAdaOnlyTxOutF)` (`libs/cardano-ledger-core/.../State/UTxO.hs`) —
per-TxOut check, vacuously `True` on an empty Foldable (so `areAllAdaOnly SNothing = True`
when there is no collateral-return output at all, via `StrictMaybe`'s `Foldable` instance).

Only the **error-report payload** (`valueWithNonAda`, used solely when `onlyAdaInCollateral`
is False and a failure IS being raised) picks a non-netted value in two of its three
branches, as described in its own source comment: raw `collateralBalance` when inputs
carry the non-ada, or the raw return `TxOut`'s value when only the return does. That
payload-selection logic is unrelated to whether the failure fires.

Contrast with `InsufficientCollateral`'s `DeltaCoin` (`bal`), which IS netted, but only
over the ada/Coin projection, from a completely separate helper
(`Cardano.Ledger.Babbage.Collateral.collAdaBalance`):
```haskell
collAdaBalance txBody utxoCollateral = toDeltaCoin $
  case txBody ^. collateralReturnTxBodyL of
    SNothing -> colbal
    SJust txOut -> colbal <-> (txOut ^. coinTxOutL @era)
  where colbal = sumAllCoin utxoCollateral   -- Coin-only sum, multi-asset ignored entirely
```
Do not conflate the two: `CollateralContainsNonADA`'s payload is a raw (non-netted, except
in the return-only-carries-non-ada branch) `Value`/`MaryValue`; `InsufficientCollateral`'s
`DeltaCoin` is a netted ada-only balance from a different function entirely.

## Q2 — DeltaCoin EncCBOR: plain signed Integer, no framing

`libs/cardano-ledger-core/src/Cardano/Ledger/Coin.hs`:
```haskell
newtype DeltaCoin = DeltaCoin Integer
  deriving (Eq, Ord, Generic, Enum, NoThunks)
  deriving (Show) via Quiet DeltaCoin
  deriving (Semigroup, Monoid, Group, Abelian) via Sum Integer
  deriving newtype (PartialOrd, NFData, ToCBOR, DecCBOR, EncCBOR, ToJSON, FromJSON)
```
`deriving newtype EncCBOR` ⇒ identical to `Integer`'s own instance
(`libs/cardano-ledger-binary/.../EncCBOR.hs`: `instance EncCBOR Integer where encCBOR =
encodeInteger`) — a bare CBOR integer, major type 0 (uint) for non-negative in
[0, 2^64-1], major type 1 (negative int) for [-2^64, -1], CBOR bignum tag 2/3 beyond that
range. No array wrapper, no group framing — `DeltaCoin` is a scalar `To`/`From` field in
the Sum encoding, same as any other single value. Note `Coin` ALSO derives EncCBOR newtype
from `Integer` (same encoder), but its DecCBOR goes through `decodeWord64` instead — an
encode/decode asymmetry specific to `Coin`, irrelevant to `DeltaCoin` (which derives both
directions newtype from `Integer`).

## Q3 — NonEmpty TxIn: bare CBOR array, tag 258 does NOT apply

`libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding/EncCBOR.hs`:
```haskell
instance EncCBOR a => EncCBOR (NonEmpty a) where
  encCBOR = encCBOR . toList
instance EncCBOR a => EncCBOR [a] where
  encCBOR = encodeList encCBOR
```
`encodeList` (`Encoder.hs`):
```haskell
encodeList encodeValue xs =
  let varLenEncList = encodeFoldableAsIndefLenList encodeValue xs
      encListVer2 = case drop lengthThreshold xs of
        [] -> encodeFoldableAsDefLenList encodeValue xs   -- <=23 elements: definite array
        _  -> varLenEncList                                -- >23: indefinite array
   in ifEncodingVersionAtLeast (natVersion @2) encListVer2 varLenEncList
lengthThreshold = 23
```
So `NonEmpty TxIn` is a plain CBOR array (major type 4), definite-length for <=23
elements / indefinite above, with NO CBOR tag prefix at all. Tag 258 belongs exclusively
to `Set`'s `EncCBOR` instance (`encodeWithSetTag`, gated `ifEncodingVersionAtLeast
(natVersion @9)` — see [[reference_cbor_set_tag_framing_audit_complete_2026_08_01]] in the
top-level MEMORY.md) and never applies to `NonEmpty` — confirms that removing an accidental
tag-258 wrapper from a `NonEmpty TxIn` payload (`BabbageNonDisjointRefInputs`) is the
byte-exact-correct fix, matches this instance exactly.

## Rust translation notes (dugite)

- `dugite-serialization`/`dugite-ledger`: any encoder for `ConwayUtxoPredFailure`-equivalent
  wire messages (N2C `MsgRejectTx` reason payloads) must use tag 12 for
  InsufficientCollateral (DeltaCoin then Coin, both plain CBOR ints, DeltaCoin can be
  negative — needs signed-int encoding, not just `encode_coin`), tag 15 for
  CollateralContainsNonADA (full MaryValue-equivalent, NOT netted — compute via the
  three-way case split above, not `inputs - return`), tag 22 for
  BabbageNonDisjointRefInputs (bare array of TxIn, no tag 258 — use the same "plain array"
  encoder as any `NonEmpty`/`Vec` field, not the Set encoder).
- Ties into open task: dugite issue #1050/#1051 collateral wire-encoder bugs
  (tx-zoo 18a/18b/18f) — this file is the byte-exact reference for that fix.
