---
name: mkpoolrewardinfo-zero-block-pool-gate
description: mkPoolRewardInfo itself (not startStep) gates on Map.lookup into BlocksMade; a zero-block pool NEVER reaches mkApparentPerformance, so mkApparentPerformance's "d>=0.8 => 1 regardless of blocksN" branch is unreachable for zero-block pools. Corrects a plausible-sounding but wrong reading of mkApparentPerformance in isolation. Live-verified 2026-08-05 @ 4849c13d6f70e5ab46add9af6e0ec5c537b61f69 (master HEAD).
metadata:
  type: reference
---

## The question this resolves

Does a registered pool with adequate pledge and nonzero stake, but ZERO blocks
produced in `bprev` that epoch, receive `maxPool'` (a full nonzero reward) when
`d >= 0.8` (pre-Babbage / TPraos overlay-heavy era), because
`mkApparentPerformance`'s `otherwise = 1` branch fires "regardless of blocksN"?

**NO.** The pool never reaches `mkApparentPerformance` at all in that state.

## Where the gate actually lives (it's NOT in startStep's iteration)

`startStep` (`eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/PulsingReward.hs`)
iterates unconditionally over **every** pool in the "go" snapshot — no
block-count pre-filter at the iteration site:
```haskell
allPoolInfo = VMap.mapWithKey mkPoolRewardInfoCurry stakePoolSnapShots
```
`stakePoolSnapShots` comes from `SnapShot activeStake totalActiveStake
stakePoolSnapShots = ssStakeGo ss` — every pool with a "go"-epoch snapshot
entry gets `mkPoolRewardInfoCurry` called on it, period.

The gate is one level deeper, INSIDE `mkPoolRewardInfo`
(`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs`):
```haskell
-- | Calculate single stake pool specific values for the reward computation.
--
-- Note that if a stake pool has made no blocks in the given epoch, it will
-- get no rewards, and so we do not need to return 'PoolRewardInfo'. We do,
-- however, need to return the relative stake of the pool in order to
-- compute data for the stake pool ranking. Eventually we will remove
-- the ranking information out of the ledger code and into a separate service,
-- and at that point we can simplify this function to not care about ranking.
mkPoolRewardInfo ::
  EraPParams era =>
  PParams era -> Coin -> BlocksMade -> Natural -> Coin -> NonZero Coin ->
  KeyHash StakePool -> StakePoolSnapShot -> Either StakeShare PoolRewardInfo
mkPoolRewardInfo pp r blocks blocksTotal (Coin totalStake) totalActiveStake
  stakePoolId stakePoolSnapShot =
    case Map.lookup stakePoolId (unBlocksMade blocks) of
      -- This pool made no blocks this epoch. For the purposes of stake pool
      -- ranking only, we return the relative stake of this pool so that we
      -- can judge how likely it was that this pool made no blocks.
      Nothing -> Left $! StakeShare sigma
      -- This pool made some blocks, so we can proceed to calculate the
      -- intermediate values needed for the individual reward calculations.
      Just numBlocksMade ->
        let Coin maxP =
              if pledge <= selfDelegatedOwnersStake
                then maxPool' pp_a0 pp_nOpt r sigma poolRelativePledge pp_maxPledgeLeverage
                else mempty
            appPerf = mkApparentPerformance pp_d sigmaA numBlocksMade blocksTotal
            poolR = rationalToCoinViaFloor (appPerf * fromIntegral maxP)
            ...
         in Right $! rewardInfo
```
`mkApparentPerformance` (and `maxPool'`/`poolR`) is called **only** inside the
`Just numBlocksMade ->` arm. A pool absent from the `BlocksMade` map takes the
`Nothing -> Left` arm and `mkApparentPerformance` is never invoked for it —
its `d_ >= 0.8 => 1` branch is architecturally unreachable in this state, not
merely numerically zero.

## Why `Just numBlocksMade` never means `Just 0` (so the gate is airtight)

`BlocksMade` is accumulated purely by increment; there is no code path that
ever inserts a zero-valued entry. `incrBlocks`
(`eras/shelley/impl/src/Cardano/Ledger/Shelley/BlockBody/Internal.hs`):
```haskell
incrBlocks block firstSlot d blocksMade@(BlocksMade blocksMadeMap)
  | isOverlay = blocksMade
  | otherwise = BlocksMade $ Map.insertWith (+) hkAsStakePool 1 blocksMadeMap
```
Starts from `Map.empty` each epoch; every application either leaves the map
untouched (overlay slot) or adds exactly 1 to an existing/absent key. By
induction every key present in the final map has value `>= 1`. So
"present in `BlocksMade`" and "made >= 1 block" are exactly equivalent —
`Map.lookup` returning `Nothing` IS the zero-block condition, with no edge
case where a zero-block pool sneaks into the `Just` arm.

## Downstream consequence — zero-block pools get NO reward, leader or member

`allPoolInfo :: VMap _ _ _ (Either StakeShare PoolRewardInfo)` is filtered:
```haskell
blockProducingPoolInfo = VMap.mapMaybe (either (const Nothing) Just) allPoolInfo
```
Both consumers key off this **filtered** (Right-only) map, not `allPoolInfo`:
- Leader rewards: `rewLeaders = VMap.foldl collectLRs mempty blockProducingPoolInfo`
  (a zero-block pool contributes nothing to `rewLeaders`).
- Member rewards: `free = FreeVars (...) totalStake (...) blockProducingPoolInfo`,
  and the pulser's per-credential step (`rewardStakePoolMember` in
  `RewardUpdate.hs`) does `poolRI <- VMap.lookup poolId fvPoolRewardInfo` inside
  the `Maybe` monad — `Nothing` short-circuits to `fromMaybe inputAnswer`,
  i.e. the credential's reward map entry is left **untouched** (no reward
  added) when its delegated pool isn't in `blockProducingPoolInfo`.

So a zero-block pool, even with `d >= 0.8`, adequate pledge, and nonzero
stake: **zero leader reward, zero reward for every delegator**. `sigma` for
that pool only ever surfaces as a `Left (StakeShare sigma)` used exclusively
by `makeLikelihoods` in `startStep` to feed the non-myopic pool-ranking
`Likelihood` calculation (wallet ranking display) — it never touches actual
Coin payouts.

## Practical upshot for a Rust port (Dugite)

An early `continue`/skip on `blocks_made_for_pool == 0` **before** computing
`appPerf`/`maxP`/`poolR` is the byte-exact-correct structure — it is not a
divergence, it matches `mkPoolRewardInfo`'s own `Nothing -> Left` gate. The
`d >= 0.8` branch inside a from-scratch port of `mkApparentPerformance` will
never be exercised with `blocksN == 0` in Haskell either, so a port is safe to
treat that combination as unreachable, PROVIDED the port's own `BlocksMade`
equivalent is built the same way (increment-only, absent key = zero, no
overlay-slot contribution — mind the `isOverlay` guard in `incrBlocks`, which
also matters independently: overlay-slot (BFT/genesis-delegate) blocks under
TPraos are **not** counted toward any stake pool's `BlocksMade` at all).

See also [[reward-calc-floor-chain-and-sigma-vs-sigmaA]] for the rest of the
reward pipeline (the 3-floor chain, sigma vs sigmaA, prevPParams usage) that
this note assumes as context.
