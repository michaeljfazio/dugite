---
name: nonmyopic-leaderprobability-precision-and-float-cbor
description: Byte-exact numeric-precision boundaries and exact input set for NonMyopic (likelihoodsNM/rewardPotNM) pool-ranking calc — leaderProbability's Rational->Double conversion points, getSigma's denominator, allPoolInfo's key set, updateNonMyopic's call site and reward-pot argument, likelihood's EpochSize arithmetic, and EncCBOR Float's unconditional 0xfa. Live-verified 2026-08-08 against cardano-node 11.0.1's exact CHaP-pinned cardano-ledger revision (see pinning method below), and diffed clean against master.
metadata:
  type: reference
---

## Source modules (IntersectMBO/cardano-ledger)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/PoolRank.hs` — `leaderProbability`, `likelihood`, `Likelihood`/`LogWeight`, `NonMyopic`
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs` — `StakeShare`, `mkPoolRewardInfo`
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/PulsingReward.hs` — `startStep`, `completeRupd`, `updateNonMyopic`
- `libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes.hs` — `PositiveUnitInterval`/`UnitInterval` (both `BoundedRatio _ Word64` = `Ratio Word64` internally), `BoundedRational`/`unboundRational`, `ActiveSlotCoeff`/`activeSlotVal`
- `libs/cardano-ledger-binary/src/Cardano/Ledger/Binary/Encoding/{EncCBOR,Encoder}.hs` — `EncCBOR Float`
- `cardano-base` (external repo) `cardano-binary/src/Cardano/Binary/ToCBOR.hs` — confirms `encodeFloat`/`encodeFloat16` are distinct, non-overlapping functions from `Codec.CBOR.Encoding` (the `cborg` package)

## Pinning method for a "what does cardano-node X.Y.Z actually run" question
cardano-node's `cabal.project` names only an `index-state` timestamp for the
`cardano-haskell-packages` (CHaP) repo, e.g. for 11.0.1:
```
index-state: cardano-haskell-packages 2026-05-02T16:21:41Z
```
There is no single "cardano-ledger commit" for a cardano-node release — CHaP
publishes each cardano-ledger sub-package (`cardano-ledger-shelley`,
`cardano-ledger-core`, `cardano-ledger-binary`, ...) independently, each
version's `_sources/<pkg>/<ver>/meta.toml` in
`IntersectMBO/cardano-haskell-packages` recording its own
`{timestamp, github.rev}`. **Resolve per-package**: take the latest version
whose `meta.toml` timestamp is `<=` the node's index-state cutoff. For 11.0.1
(cutoff 2026-05-02T16:21:41Z):
- `cardano-ledger-shelley` 1.18.1.0, rev `b7c17cf31871062b7883c46e3f367cb5e1b5db6c`, published 2026-04-13T13:33:52Z (next version 1.19.0.0 published 2026-07-29, after cutoff — excluded)
- `cardano-ledger-core` 1.20.0.0 and `cardano-ledger-binary` 1.8.1.0, BOTH rev `94e9618c91a16ec08db477632a158b630722089b`, published 2026-04-13T10:18:15Z

Note the shelley-package rev and the core/binary-package rev are **different
commits**, ~3 hours apart on the same release day — normal, since each
package's publish captures whatever the monorepo HEAD was at *that package's*
bump, not a synchronized monorepo-wide tag. Don't assume one rev covers the
whole dependency graph.

**Diffed clean**: `PoolRank.hs` and `PulsingReward.hs` are byte-identical
between this pinned revision and current `master`. `Encoder.hs`'s
`encodeFloat`/`encodeDouble`/`encodeFloat16` bodies are identical (only
shifted a few line numbers by unrelated additions elsewhere in the file).
`BaseTypes.hs`'s `PositiveUnitInterval`/`UnitInterval` newtypes and their
`Bounded` instances are unchanged in substance (master inserted a new
`PositiveInterval`/`NonNegativeInterval` pair earlier in the file, shifting
position only). **For this specific calculation, master and the 11.0.1 pin
agree in every particular below** — unlike the SnapShot record history, there
is no drift to flag here.

## 1. `leaderProbability` — verbatim, with exact conversion boundary
```haskell
leaderProbability :: ActiveSlotCoeff -> Rational -> UnitInterval -> Double
leaderProbability activeSlotCoeff relativeStake decentralizationParameter =
  (1 - (1 - asc) ** s) * (1 - d')
  where
    d' = realToFrac . unboundRational $ decentralizationParameter
    asc = realToFrac . unboundRational . activeSlotVal $ activeSlotCoeff
    s = realToFrac relativeStake
```
- `activeSlotVal :: ActiveSlotCoeff -> PositiveUnitInterval` is a bare field
  accessor (`unActiveSlotVal`), no arithmetic.
- **`realToFrac`, not `fromRational`**, converts at all three boundaries.
  Behaviorally identical for a `Rational` source (`realToFrac = fromRational
  . toRational`, and `toRational` on a `Rational` is `id`) — but the literal
  source says `realToFrac`; don't "correct" a port that also says
  `realToFrac` back to `fromRational` thinking it's clearer, and don't assume
  the two are interchangeable for OTHER types elsewhere in the codebase where
  the source type isn't already `Rational`.
- `PositiveUnitInterval`/`UnitInterval` both newtype-wrap `BoundedRatio _
  Word64` — i.e. internally a `Ratio Word64` (both numerator and denominator
  bounded to `Word64` range). `unboundRational` = `toRationalBoundedRatio` =
  `promoteRatio`, an EXACT, lossless promotion of that `Ratio Word64` into
  arbitrary-precision `Rational` (`Ratio Integer`). So the only lossy step in
  the entire chain is the single `realToFrac :: Rational -> Double` call —
  everything upstream of it (PParams decode, `%.`/`%?` construction of
  `sigma`) is exact rational arithmetic.
- `**` is `Prelude.(**)` (`Floating Double`'s method) — PoolRank.hs imports
  nothing that shadows it, confirmed from its own import list (only
  `Cardano.Ledger.BaseTypes`, `.Binary`, `.Coin`, `.Core`, `.Keys`,
  `.Shelley.Rewards`, `.State`, `Cardano.Slotting.Slot`, stdlib containers —
  no custom `Prelude` hiding).
- `s = realToFrac relativeStake` — the `Rational` argument is `sigma` (NOT
  `sigmaA`; see `getSigma` below), fed straight in with no intermediate
  clamping.

## 2. `StakeShare` / `getSigma` — denominator is `totalStake`, not active stake
```haskell
-- Rewards.hs
newtype StakeShare = StakeShare {unStakeShare :: Rational}
```
`getSigma` is a local binding inside `startStep`, not an exported function:
```haskell
-- PulsingReward.hs, inside startStep
getSigma = unStakeShare . poolRelativeStake
```
applied to the `Right info :: PoolRewardInfo` branch. `poolRelativeStake` is
set in `mkPoolRewardInfo` (Rewards.hs) as `StakeShare sigma` — the SAME
`sigma` used for `maxPool'`/`calcStakePoolOperatorReward`/
`calcStakePoolMemberReward`, **not** `sigmaA` (the apparent-performance-only
normalizer). From `mkPoolRewardInfo`'s `where` clause:
```haskell
sigma  = poolTotalStake %? totalStake
sigmaA = poolTotalStake %. unCoinNonZero totalActiveStake
```
`totalStake` is `mkPoolRewardInfo`'s own 5th positional argument, which
`startStep` supplies as:
```haskell
totalStake = circulation es maxSupply
-- circulation (EpochState acnt _ _ _) supply = supply <-> casReserves acnt
```
i.e. **current-epoch circulating supply** (`maxSupply - casReserves` off the
live `EpochState`'s account state at reward-calc kickoff), the exact same
`totalStake` fed to `mkPoolRewardInfoCurry` for every pool. It is NOT
`totalActiveStake`/`pdTotalActiveStake` (that's `sigmaA`'s denominator,
used only inside `mkApparentPerformance`) and NOT a fixed max-Lovelace
constant. See also [[reward-calc-floor-chain-and-sigma-vs-sigmaA]] for the
full sigma-vs-sigmaA writeup (this note only adds the `getSigma`
call-site detail that memory doesn't cover).

For the `Left (StakeShare sigma)` branch (zero-block pool), it's the exact
same `sigma` binding (computed once in `mkPoolRewardInfo`'s shared `where`
clause, used regardless of which constructor is returned) — so a zero-block
pool's likelihood calc uses the identical stake normalizer a block-producing
pool would have used, just with `blocks = 0` hardcoded (see #4).

## 3. `allPoolInfo` — full verbatim, key set = every pool in the go snapshot
```haskell
-- startStep, PulsingReward.hs
let SnapShot activeStake totalActiveStake stakePoolSnapShots = ssStakeGo ss
    ...
    mkPoolRewardInfoCurry =
      mkPoolRewardInfo pr _R b (fromIntegral blocksMade) totalStake totalActiveStake
    allPoolInfo = VMap.mapWithKey mkPoolRewardInfoCurry stakePoolSnapShots
    blockProducingPoolInfo = VMap.mapMaybe (either (const Nothing) Just) allPoolInfo
```
`allPoolInfo :: VMap _ _ (KeyHash StakePool) (Either StakeShare
PoolRewardInfo)` — key set is **every key present in `ssStakeGo`'s
`stakePoolSnapShots` VMap**, unconditionally, regardless of stake amount or
block count. `VMap.mapWithKey` maps every key; there is no pre-filter at this
call site. The `Left`/`Right` split happens ONE LEVEL DEEPER, inside
`mkPoolRewardInfo` itself, keyed on `Map.lookup stakePoolId (unBlocksMade
blocks)` — `Nothing` (0 blocks) -> `Left (StakeShare sigma)`, `Just n` (>=1
block, since `BlocksMade` is increment-only) -> `Right PoolRewardInfo`. See
[[mkpoolrewardinfo-zero-block-pool-gate]] for the full zero-block-pool
mechanics this reuses.

## 4. `startStep`'s `newLikelihoods`, and `updateNonMyopic`'s call site + reward-pot argument
```haskell
-- startStep, PulsingReward.hs
makeLikelihoods = \case
  Left (StakeShare sigma) ->
    likelihood 0 (leaderProbability asc sigma $ pr ^. ppDG) slotsPerEpoch
  Right info ->
    likelihood (poolBlocks info) (leaderProbability asc (getSigma info) $ pr ^. ppDG) slotsPerEpoch
newLikelihoods = VMap.map makeLikelihoods allPoolInfo
```
- Left branch: `blocks` argument to `likelihood` is the LITERAL `0`, not
  `poolBlocks`-derived (there is no `poolBlocks` in scope for `Left`).
- Both branches pass `asc` (startStep's own `ActiveSlotCoeff` parameter — a
  global protocol constant, the caller's, not per-pool) and `pr ^. ppDG`
  (decentralization parameter from the **previous epoch's** PParams, same
  `pr` used throughout `startStep`'s reward math).
- `slotsPerEpoch` is `startStep`'s own `EpochSize` parameter, passed straight
  through unmodified to both branches (see #5).

`newLikelihoods` is stashed into `RewardSnapShot { rewLikelihoods =
newLikelihoods, ... }` in Phase 1 (`startStep`), but **`updateNonMyopic` is
NOT called there**. It is called exactly once, in Phase 3 (`completeRupd`):
```haskell
-- completeRupd, PulsingReward.hs
completeRupd (Pulsing RewardSnapShot{ rewDeltaR1 = deltaR1, rewFees = feesSS,
  rewR = oldr, rewDeltaT1 = Coin deltaT1, rewNonMyopic = nm,
  rewLikelihoods = newLikelihoods, rewLeaders = lrewards,
  rewProtocolVersion = protVer } pulser) = do
  ...
  pure ( RewardUpdate
           { deltaT = DeltaCoin deltaT1
           , deltaR = invert (toDeltaCoin deltaR1) <> toDeltaCoin deltaR2
           , rs = rs''
           , deltaF = invert (toDeltaCoin feesSS)
           , nonMyopic = updateNonMyopic nm oldr newLikelihoods
           }
       , newevent )
```
`oldr` is `rewR` off the `RewardSnapShot`, which `startStep` set as:
```haskell
Coin rPot = ssFee ss <> deltaR1
deltaT1 = floor $ unboundRational (pr ^. ppTauL) * fromIntegral rPot
_R = Coin $ rPot - deltaT1
-- rewsnap = RewardSnapShot { ..., rewR = _R, ... }
```
So **`rewardPotNM` becomes exactly `_R = rPot - deltaT1`** — the epoch's
total reward pot AFTER the treasury cut (`deltaT1`) is subtracted, where
`rPot = fees + deltaR1` (fees plus the floored reserves draw). It is NOT
`rPot` itself, NOT `deltaR1`, NOT the treasury cut. `updateNonMyopic`'s own
body:
```haskell
updateNonMyopic nm rPot_ newLikelihoods =
  nm { likelihoodsNM = updatedLikelihoods, rewardPotNM = rPot_ }
  where
    history = likelihoodsNM nm
    performance kh newPerf =
      maybe mempty (applyDecay decayFactor) (VMap.lookup kh history) <> newPerf
    updatedLikelihoods = VMap.mapWithKey performance newLikelihoods
decayFactor :: Float
decayFactor = 0.9
```
`nm` (the OLD `NonMyopic`, decay source) is `rewNonMyopic` off the same
`RewardSnapShot`, which `startStep` captured as the incoming `EpochState`'s
own `nonMyopic` field (`es@(EpochState acnt ls ss nm)` — the 4th component,
same-named `nm`) — i.e. the value going INTO this epoch's calc, before this
epoch's update. `updatedLikelihoods`'s key set is that of `newLikelihoods`
(this epoch's go-snapshot pools), NOT a union with `history` — a pool that
drops out of the go snapshot (retired/deregistered) simply disappears from
`likelihoodsNM`, it is not merged forward. Decay (`applyDecay decayFactor`,
i.e. `* 0.9` on every `LogWeight`) is applied ONLY to the looked-up OLD value,
never to `newPerf`.

`updateNonMyopic` is reachable from two call paths, both ultimately through
`completeRupd`: `completeStep -> completeRupd` (when a prior `pulseStep`
detects `done pulser`), and `createRUpd`'s own fallback
(`Pulsing rewsnap pulser -> fst <$> completeRupd (...)`). There is no path
that skips it once a `RewardUpdate` is finalized, and no path that calls it
from `startStep`.

## 5. `EpochSize` in `likelihood` — full epoch length, Word64 subtraction
```haskell
likelihood :: Natural -> Double -> EpochSize -> Likelihood
likelihood blocks t slotsPerEpoch =
  Likelihood $ sample <$> samplePositions
  where
    n = fromIntegral blocks
    m = fromIntegral $ unEpochSize slotsPerEpoch - fromIntegral blocks
    l :: Double -> Double
    l x = n * log x + m * log (1 - t * x)
    sample position = LogWeight (realToFrac $ l position)
```
- `slotsPerEpoch :: EpochSize` is `startStep`'s own parameter, passed through
  unmodified from both `makeLikelihoods` branches — confirmed the FULL epoch
  length (`Cardano.Slotting.Slot.EpochSize`, `newtype EpochSize {unEpochSize
  :: Word64}`), not an active-slot-adjusted or windowed value.
- `unEpochSize slotsPerEpoch - fromIntegral blocks` is computed in **`Word64`
  arithmetic** — `unEpochSize` yields `Word64`, and `fromIntegral blocks`
  (from `Natural`) is unified to the same type by `(-)`'s type, BEFORE the
  outer `fromIntegral` promotes the *result* to `Double` for `m`. This means
  the subtraction is UNSIGNED: it cannot produce a negative intermediate by
  construction, and there is no explicit bounds check — if the invariant
  `blocks <= unEpochSize slotsPerEpoch` were ever violated (it can't be in a
  correct ledger, since a pool cannot produce more blocks than there are
  slots in an epoch), the subtraction would **wrap** (Word64 underflow) to a
  huge value, not throw or clamp to zero. A Rust port should mirror this as
  unsigned arithmetic under the same invariant, not as a checked/saturating
  subtraction that changes behavior on a violated invariant (that would be a
  divergence from Haskell's silent-wrap semantics, even though the invariant
  should make it unreachable either way).
- `n = fromIntegral blocks` — direct `Natural -> Double`, no rational
  intermediate.

## 6. `EncCBOR Float` — unconditional single-precision, no shortest-form logic
```haskell
-- cardano-ledger-binary Encoder.hs
encodeFloat16 :: Float -> Encoding
encodeFloat16 e = fromPlainEncoding (C.encodeFloat16 e)
encodeFloat :: Float -> Encoding
encodeFloat e = fromPlainEncoding (C.encodeFloat e)
encodeDouble :: Double -> Encoding
encodeDouble e = fromPlainEncoding (C.encodeDouble e)

-- cardano-ledger-binary EncCBOR.hs
instance EncCBOR Float where
  encCBOR = encodeFloat
instance EncCBOR Double where
  encCBOR = encodeDouble
```
`C.encodeFloat`/`C.encodeFloat16` resolve through `Cardano.Binary` (the
external `cardano-base` repo) to `Codec.CBOR.Encoding.encodeFloat` /
`.encodeFloat16` respectively (confirmed via `cardano-base`'s
`cardano-binary/src/Cardano/Binary/ToCBOR.hs`: `instance ToCBOR Float where
toCBOR = E.encodeFloat` with `import Codec.CBOR.Encoding as E`) — these are
the `cborg` package's own primitives, and they are **two entirely separate,
non-overlapping token constructors** (`TkFloat16`/`TkFloat32` equivalents),
each always emitting its own fixed-width form. `encodeFloat` has NO
value-dependent branching — it does not check whether the `Float` would
round-trip through a half-precision encoding and never delegates to
`encodeFloat16`. `LogWeight` derives its `EncCBOR` instance via
`GeneralizedNewtypeDeriving` directly off `Float`, so **every `LogWeight` in
a `Likelihood`'s 100-entry `StrictSeq`, at any magnitude, is CBOR major type
7 additional-info 26 (`0xfa`) followed by 4 bytes IEEE-754 single-precision,
unconditionally** — confirms a wire capture showing `0xfa` is architecturally
guaranteed, not incidental to the specific values captured.

## Practical upshot for a Rust port (Dugite)
1. `leaderProbability`: do the `sigma`/`d`/`asc` arithmetic in exact
   arbitrary-precision rational (e.g. `num_rational::BigRational` or
   equivalent) right up to the point of conversion, then convert each of the
   three inputs to `f64` independently (mirroring three separate
   `realToFrac` calls) BEFORE the `(1 - (1-asc)**s) * (1-d)` formula, which
   itself runs entirely in `f64`/`**` (`f64::powf`) — not before.
2. `getSigma`/`sigma` (as opposed to `sigmaA`) is `pool_stake / (max_supply -
   current_reserves)`, both operands taken from the CURRENT `EpochState` at
   reward-calc kickoff, using `%?` safe-divide (0 if denominator 0) — not
   `pdTotalActiveStake`.
3. Iterate every pool key present in the go-snapshot's pool map
   unconditionally when building `allPoolInfo`'s Rust equivalent; do the
   0-blocks-vs-N-blocks split inside the per-pool function, not as a
   pre-filter over the iteration.
4. `rewardPotNM` must be set from the SAME `Coin` that becomes `_R` in the
   reward-pot chain (`fees + deltaR1 - deltaT1`), captured once in Phase 1
   and threaded unchanged to wherever `update_non_myopic`'s Rust equivalent
   runs — and that update must happen at the equivalent of Phase 3
   (pulser-complete), never inline during Phase 1's pool iteration.
5. `EpochSize` for `likelihood` is the untouched full per-epoch slot count;
   compute `m` as an unsigned subtraction under the `blocks <= epoch_len`
   invariant (don't silently switch to saturating/checked subtraction
   semantics that would diverge from Haskell's wrap-on-violation behavior).
6. Every `LogWeight`/`f32` in a `Likelihood` on the wire is 4-byte
   IEEE-754 single precision (`0xfa` + 4 bytes), unconditionally — no
   shortest-form CBOR float logic exists anywhere in this path to replicate.

## Related
[[reward-calc-floor-chain-and-sigma-vs-sigmaA]] — full 3-floor Coin-reward
chain (this note only covers the separate NonMyopic/pool-ranking path, which
shares `sigma` and `pr`/prevPParams but produces no Coin payouts itself).
[[mkpoolrewardinfo-zero-block-pool-gate]] — exact mechanics of the
`Left`/`Right` split this note's `allPoolInfo`/`makeLikelihoods` section
builds on.
