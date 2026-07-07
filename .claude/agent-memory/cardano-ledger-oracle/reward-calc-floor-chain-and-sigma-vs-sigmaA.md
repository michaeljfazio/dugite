---
name: reward-calc-floor-chain-and-sigma-vs-sigmaA
description: Byte-exact reward formula chain (maxPool' -> poolR -> leader/member reward), the two DISTINCT sigma values (sigma vs sigmaA) and their separate uses, prevPParams usage, and pre/post-floor filtering. Live-verified 2026-07-07 against Cardano.Ledger.Shelley.Rewards + Cardano.Ledger.State.SnapShots + PulsingReward.hs.
metadata:
  type: reference
---

## Source modules (all IntersectMBO/cardano-ledger, master, live-verified 2026-07-07)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs` — `mkApparentPerformance`, `calcStakePoolOperatorReward`, `calcStakePoolMemberReward`, `rewardOnePoolMember`, `mkPoolRewardInfo`
- `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs` — `maxPool'`
- `libs/cardano-ledger-core/src/Cardano/Ledger/Coin.hs` — `rationalToCoinViaFloor = Coin . floor` (also `rationalToCoinViaCeiling = Coin . ceiling`, used elsewhere e.g. deltaT1/deltaR1 is floor too, not ceiling)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/PulsingReward.hs` — `startStep` (caller of `mkPoolRewardInfo`, computes `_R`, `totalStake`, `deltaR1`/`deltaT1`), `circulation`
- `libs/cardano-ledger-core/src/Cardano/Ledger/BaseTypes/NonZero.hs` — `(%.)` (divide by statically-nonzero denom), `(%?)` (safe divide, returns 0 if denom is 0)

## The exact floor/rounding chain (3 independent floor operations, in order)

**Floor 1 — `maxPool'` (max reward available to a pool, before performance adjustment):**
```haskell
maxPool' a0 nOpt r sigma pR = rationalToCoinViaFloor $ factor1 * factor2
  where
    z0 = 1 / nOpt                          -- recipNonZero . toRatioNonZero
    sigma' = min sigma z0
    p'     = min pR z0
    factor1 = coinToRational r / (1 + unboundRational a0)
    factor2 = sigma' + p' * unboundRational a0 * factor3
    factor3 = (sigma' - p' * factor4) /. nonZeroZ0
    factor4 = (z0 - sigma') /. nonZeroZ0
```
`r` here is `_R`, the epoch's total reward pot (`_R = rPot - deltaT1`, itself `Coin $ ...` built from
TWO prior independent floors: `deltaR1 = floor(min(1,eta) * rho * reserves)` and
`deltaT1 = floor(tau * rPot)` where `rPot = fees + deltaR1` — see `startStep` in PulsingReward.hs).
`sigma` passed in here is the pool's stake-over-**totalStake** ratio (NOT sigmaA, see below).

This gate precedes it entirely — if the pool doesn't meet its own declared pledge, `maxP = mempty`
(zero), skipping `maxPool'` altogether:
```haskell
Coin maxP =
  if pledge <= selfDelegatedOwnersStake
    then maxPool' pp_a0 pp_nOpt r sigma poolRelativePledge
    else mempty
```
`pledge`/`selfDelegatedOwnersStake` both come from the **"go" `StakePoolSnapShot`** (2-epochs-stale),
not current pool registration state. Comparison is `<=` (pledge exactly met passes).

**Floor 2 — `mkPoolRewardInfo` (pool's actual awarded pot this epoch, after performance):**
```haskell
appPerf = mkApparentPerformance pp_d sigmaA numBlocksMade blocksTotal
poolR   = rationalToCoinViaFloor (appPerf * fromIntegral maxP)
```
This is a **second, separate** floor over `appPerf * maxP` — not `floor(appPerf) * maxP` and not two
sequential per-factor floors. `maxP` (already an Integer from Floor 1) is promoted back to Rational
via `fromIntegral` before this multiply.

**`mkApparentPerformance` uses `sigmaA`, not `sigma`** — this is the single most important, easy-to-miss
distinction in the whole chain:
```haskell
mkApparentPerformance d_ sigma blocksN blocksTotal
  | sigma == 0 = 0
  | unboundRational d_ < 0.8 = beta / sigma
  | otherwise = 1
  where beta = toInteger blocksN % toInteger (max 1 blocksTotal)
```
called as `mkApparentPerformance pp_d sigmaA numBlocksMade blocksTotal` — the `sigma` parameter name
inside `mkApparentPerformance` is filled with **`sigmaA`** at the call site, NOT the `sigma` used
everywhere else in the same function. Two distinct ratios exist side by side in `mkPoolRewardInfo`:

```haskell
sigma  = poolTotalStake %? totalStake         -- used for: maxPool' relative-stake term,
                                               -- calcStakePoolOperatorReward, calcStakePoolMemberReward
sigmaA = poolTotalStake %. unCoinNonZero totalActiveStake   -- used ONLY inside mkApparentPerformance
```
- `totalStake = circulation es maxSupply = maxSupply - casReserves acnt` — i.e. **current circulating
  supply** (maxLovelaceSupply minus reserves *as of reward-calc kickoff*, using the CURRENT
  `EpochState`'s account state — not the "go"-epoch reserves value).
- `totalActiveStake` comes bundled in the **"go" `SnapShot`** itself (`ssStakeGo`'s
  `totalActiveStake` field) — the sum of stake actually attributed to registered pools in that
  snapshot, i.e. a subset of `totalStake` (excludes undelegated ADA).

Conflating these two (using the same normalizer for both apparent-performance AND reward-share
calculations) is a plausible, concrete, byte-exact-breaking bug class: it would systematically bias
every pool's `appPerf` (and therefore every member's and every leader's reward) by the ratio
`totalActiveStake/totalStake`, producing small per-account errors that don't show up as a gross
sanity-check failure (order of magnitude still right) but DO break exact equality — exactly the
symptom described as "79 lovelace short."

**Protocol parameters used (`a0`, `nOpt`, `d`, `rho`, `tau`) are all taken from the PREVIOUS epoch's
PParams**, not the current epoch's:
```haskell
pr = es ^. prevPParamsEpochStateL
```
and `mkPoolRewardInfoCurry = mkPoolRewardInfo pr _R b (fromIntegral blocksMade) totalStake totalActiveStake`
— i.e. `pp` inside `mkPoolRewardInfo` (from which `pp_d`/`pp_a0`/`pp_nOpt` are read) is bound to `pr`.
Using current-epoch PParams instead of previous-epoch PParams is a second plausible, concrete bug
source — it would only manifest as a discrepancy in epochs immediately following (or straddling) a
governance-enacted PParams change, and would otherwise be invisible.

**Floor 3 (x2) — leader reward and each member reward, independently, both from the SAME `poolR`:**
```haskell
calcStakePoolOperatorReward f cost margin (StakeShare s) (StakeShare sigma)
  | f <= cost = f
  | otherwise = cost <> rationalToCoinViaFloor (coinToRational (f <-> cost) * (m + (1 - m) * s / sigma))

calcStakePoolMemberReward (Coin f) (Coin cost) margin (StakeShare t) (StakeShare sigma)
  | f <= cost = mempty
  | otherwise = rationalToCoinViaFloor $ fromIntegral (f - cost) * (1 - m) * t / sigma
```
Both take `f = poolR` (Floor 2's result) and the pool's own `cost`/`margin` (from the "go"
`StakePoolSnapShot`, i.e. `spssCost`/`spssMargin`). `s`/`t` are `StakeShare`s built as raw
`Rational`s (`c % unCoin totalStake` for a member's own stake `c`) — NOT pre-divided by pool stake;
the division by pool stake happens via `.../sigma` in the same expression, algebraically equal to
`memberStake/poolStake` but computed as `(memberStake/totalStake) / (poolStake/totalStake)`. Because
Haskell `Rational` is exact arbitrary-precision (auto-reduced via GCD by the `%` constructor), this
path is **value-identical** to computing `memberStake/poolStake` directly — the risk is not "wrong
order of operations" per se, it's (a) any place Dugite substitutes floating point or an intermediate
truncation for an otherwise-exact rational, or (b) an extra/missing floor at an intermediate step
that Haskell doesn't have.

- Leader reward: if `poolR <= cost`, leader gets the **entire** `poolR` (no floor needed, exact
  passthrough) — else `cost + floor((poolR-cost)*(margin + (1-margin)*ownerStake/sigma))`, addition
  via `Coin`'s Monoid (`<>` = integer addition) happens **after** the floor, not before.
- Member reward: if `poolR <= cost`, member gets **`mempty` (Coin 0)**, not `f` — asymmetric with the
  leader case. Else `floor((poolR-cost)*(1-margin)*memberStake/(totalStake*sigma))`, independently
  per member.

## Answer to "dropped before or after the floor" (point (d) in the original question)

**After.** `rewardOnePoolMember` always computes the floored `r` first via `calcStakePoolMemberReward`,
then applies filtering:
```haskell
rewardOnePoolMember pv totalStake addrsRew rewardInfo hk (Coin c) =
  if prefilter && notPoolOwner (...) hk && r /= Coin 0
    then Just r
    else Nothing
  where
    prefilter = hardforkBabbageForgoRewardPrefilter pv || hk `Set.member` addrsRew
    r = calcStakePoolMemberReward poolR spssCost spssMargin stakeShare sigma
```
Pre-Babbage-hardfork, `prefilter` requires the credential to be in the currently-registered-accounts
set (`addrsRew`) or the reward is dropped entirely regardless of the floored value. **From the
Babbage hardfork onward (`hardforkBabbageForgoRewardPrefilter`), this registration prefilter is
unconditionally `True`** — every credential in the "go" stake distribution gets a reward computed
and kept (subject only to `notPoolOwner` and `r /= Coin 0`), and filtering by current registration
status is deferred to a later stage (reward-crediting against the live `Accounts`/UMap, not this
function). Era-gating this correctly matters for any era before Babbage.

## Audit checklist for a Rust reimplementation (Dugite) given a small (tens-of-lovelace) per-account drift
1. Confirm `sigma` (reward-share normalizer, `poolStake %? totalStake`) and `sigmaA` (apparent-performance
   normalizer, `poolStake %. totalActiveStake`) are two distinct values fed from two distinct sources —
   NOT the same variable reused.
2. Confirm `totalStake = maxSupply - currentReserves` (current `EpochState` reserves at reward-calc
   time) and `totalActiveStake` comes from the "go" snapshot's own stored total, not recomputed from
   `totalStake`.
3. Confirm all of `a0, nOpt, d, rho, tau` used in the reward calc are the **previous** epoch's enacted
   PParams, not current.
4. Confirm exactly 3 independent floor stages (maxPool' -> poolR -> leader/member), each using exact
   rational arithmetic (arbitrary-precision numerator/denominator, not floating point or an
   intermediate `Coin`-typed rounding).
5. Confirm the pledge-vs-owner-stake gate (`pledge <= selfDelegatedOwnersStake`) uses "go"-snapshot
   values and `<=` (not `<`), zeroing `maxP` entirely (not partially) when it fails.
6. Confirm leader reward is a *plain passthrough* of `poolR` (no floor) when `poolR <= cost`, while
   member reward becomes exactly `0` (not passthrough) in the same condition — asymmetric by design.
7. Confirm registration-based reward filtering is era-gated (`hardforkBabbageForgoRewardPrefilter`) —
   pre-Babbage drops unregistered members' rewards entirely; Babbage+ keeps them (deferred filtering).
