# Shelley-Era Reward Calculation Pipeline — Reference

Source base: `IntersectMBO/cardano-ledger` master @ ~2026-05.

Canonical files:

| File | Role |
|---|---|
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Rupd.hs` | STS RUPD rule — slot-timing gate, pulser lifecycle |
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Snap.hs` | STS SNAP rule — snapshot rotation |
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/PulsingReward.hs` | `startStep` / `pulseStep` / `completeStep` / `completeRupd` |
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs` | `mkPoolRewardInfo`, `rewardOnePoolMember`, `calcStakePool*Reward` |
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/RewardUpdate.hs` | `RewardUpdate`, `FreeVars`, `RewardAns`, `RewardPulser (RSLP)` |
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/IncrementalStake.hs` | `applyRUpd`, `applyRUpdFiltered`, `filterAllRewards'` |
| `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs` | `SnapShots`, `SnapShot`, `StakePoolSnapShot`, `maxPool'`, `calculatePoolDistr` |
| `libs/cardano-ledger-core/src/Cardano/Ledger/State/Stake.hs` | `ActiveStake`, `StakeWithDelegation`, `resolveActiveInstantStakeCredentials` |
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/NewEpoch.hs` | NEWEPOCH transition — `updateRewards`, `applyRUpd` sequencing |
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/Era.hs` | Hardfork predicate functions |
| `eras/shelley/impl/src/Cardano/Ledger/Shelley/AdaPots.hs` | `totalAdaPotsES`, conservation invariant |

---

## 1. RUPD Rule: Slot-Timing Gate and Pulser Lifecycle

### 1.1 Stability window and slot boundaries

The RUPD rule (`ShelleyRUPD era`) is invoked on every block. Its signal is the current slot. It computes:

```haskell
sr <- asks randomnessStabilisationWindow    -- = 4k/f (from genesis config)
let e = epochInfoEpoch ei s
    slotsPerEpoch = epochInfoSize ei e
    slot      = epochInfoFirst ei e +* Duration sr   -- start-of-epoch + 4k/f
    slotForce = slot                       +* Duration sr   -- start-of-epoch + 8k/f
```

- `slot` = first slot of epoch + `randomnessStabilisationWindow` (= 4k/f). Earliest slot where reward calc may begin.
- `slotForce` = first slot + 8k/f. Past this the pulser is forced complete.

### 1.2 Goldilocks labelling

```haskell
determineRewardTiming currentSlot startAfterSlot endSlot
  | currentSlot > endSlot          = RewardsTooLate
  | currentSlot <= startAfterSlot  = RewardsTooEarly
  | otherwise                      = RewardsJustRight
```

- **`RewardsTooEarly`** (s ≤ epoch_start + 4k/f): randomness not yet stable. RUPD state forced to `SNothing` — any in-progress pulser is discarded.
- **`RewardsJustRight`** (4k/f < s ≤ 8k/f): normal. If state `SNothing` → `startStep`. If `Pulsing` → `pulseStep`. If `Complete` → no-op.
- **`RewardsTooLate`** (s > 8k/f): force completion. If `SNothing` → `startStep` then `completeStep`. If `Pulsing` → `completeStep`.

Critical: `RewardsTooEarly` sets state to `SNothing`, not `SJust (Pulsing ...)`. Nothing started before the stability window survives.

### 1.3 RUPD state type

```haskell
data PulsingRewUpdate
  = Pulsing !RewardSnapShot !Pulser
  | Complete !RewardUpdate
```

RUPD STS state is `StrictMaybe PulsingRewUpdate`. At each epoch start it begins as `SNothing`.

---

## 2. SNAP Rule: Snapshot Rotation

### 2.1 When SNAP fires

SNAP is invoked by EPOCH at each epoch boundary, **after POOLREAP** and **before** the pool-distribution update. Internal STS with signal `()`.

### 2.2 Rotation semantics (exact, from `snapTransition`)

```haskell
pure $ SnapShots
  { ssStakeMark          = istakeSnap           -- NEW: from current instantStake
  , ssStakeMarkPoolDistr = calculatePoolDistr istakeSnap
  , ssStakeSet           = ssStakeMark s        -- OLD mark → new set
  , ssStakeGo            = ssStakeSet  s        -- OLD set → new go
  , ssFee                = fees                 -- drained from utxosFeesL
  }
```

After SNAP at boundary N→(N+1):

| Name | Represents stake from epoch |
|---|---|
| `ssStakeMark` | N (freshly computed at end of N) |
| `ssStakeSet`  | N-1 |
| `ssStakeGo`   | N-2 |

**Invariant:** `ssStakeGo` at the boundary entering epoch E always contains the stake snapshot taken at the boundary entering epoch E-2.

### 2.3 What `istakeSnap` captures

`istakeSnap = snapShotFromInstantStake instantStake dstate pstate`

- `instantStake`: incrementally maintained — every UTxO spend/creation during epoch N is reflected.
- `snapShotFromInstantStake` → `resolveInstantStake` → `resolveActiveInstantStakeCredentials`. Merges instant stake with account balances and **filters to credentials that are registered AND delegated to a pool**.
- Pointer addresses: in Conway, `addConwayInstantStake` drops `StakeRefPtr` entirely; in Shelley-Babbage, pointer-keyed UTxO stake is in `instantStake` but only appears in active stake if the pointer maps to a registered, delegated key.
- Pool params come from `pstate` (`psStakePools`) at the moment of snapshot.
- `ssFee` = `utxosFeesL` — accumulated tx fees for epoch N.

### 2.4 `ssStakeMarkPoolDistr` memoization (ADR-7)

```haskell
ssStakeMarkPoolDistr = calculatePoolDistr istakeSnap
```

NEWEPOCH then assigns it to `nesPd` without recomputing:

```haskell
let pd' = ssStakeMarkPoolDistr (esSnapshots es)
```

So `nesPd` (next epoch's VRF leader check pool distribution) = `calculatePoolDistr(mark_at_boundary_N)` = `calculatePoolDistr(set_at_boundary_N+1)`.

---

## 3. PulsingReward: startStep, pulseStep, completeStep

### 3.1 `startStep` — Phase 1

```haskell
startStep
  :: EpochSize
  -> BlocksMade               -- = nesBprev
  -> EpochState era
  -> Coin                     -- = maxLovelaceSupply
  -> ActiveSlotCoeff          -- = f
  -> NonZero Word64           -- = securityParam k
  -> PulsingRewUpdate
```

**Step 1 — source snapshot:**
```haskell
SnapShot activeStake totalActiveStake stakePoolSnapShots = ssStakeGo ss
```
The reward calc uses the **go** snapshot (= stake from 2 epochs ago). Rewards for epoch N are paid using stake from epoch N-2.

**Step 2 — pulse size:**
```haskell
numStakeCreds = fromIntegral (VMap.size $ unActiveStake activeStake)
pulseSize = max 1 (ceiling (numStakeCreds %. (knownNonZero @4 `mulNonZero` k)))
```
Processes `ceil(numStakeCreds / (4k))` credentials per block. With mainnet (k=2160, ~21600 slots/epoch, f=0.05): `ceil(numCreds / 8640)` per block.

**Step 3 — monetary expansion (`deltaR1`):**
```haskell
pr = es ^. prevPParamsEpochStateL          -- ← uses PREVIOUS epoch's PParams
deltaR1 =
  rationalToCoinViaFloor $
    min 1 eta * unboundRational (pr ^. ppRhoL) * fromIntegral reserves
```

Where:
- `reserves` = `casReserves acnt`
- `pr ^. ppRhoL` = ρ from **prevPParams** (critical for parity — the PParams active during epoch N are at `prevPParams` from start of epoch N+1)
- `eta` = apparent performance:
  ```haskell
  expectedBlocks = floor $ (1 - d) * f * slotsPerEpoch
  blocksMade = fromIntegral $ Map.foldr (+) 0 b'
  eta
    | d >= 0.8  = 1
    | otherwise = blocksMade % expectedBlocks
  ```
  - When d ≥ 0.8: `eta = 1` regardless of actual block production (federated).
  - When d < 0.8: `eta = actualBlocks / expectedBlocks`, capped at 1 by `min 1 eta`.
  - In Conway where d ≡ 0: `expectedBlocks = floor(f * slotsPerEpoch)`.

**Step 4 — reward pot:**
```haskell
Coin rPot = ssFee ss <> deltaR1            -- ssFee = ssFee field of GO snapshot
deltaT1   = floor $ unboundRational (pr ^. ppTauL) * fromIntegral rPot
_R        = Coin $ rPot - deltaT1
```
`ssFee ss` is the **go** snapshot's `ssFee` (two-epoch lag — see §8.4 below). Not the current epoch's fees.

**Step 5 — pool info and leader rewards:**
```haskell
totalStake = circulation es maxSupply           -- = maxSupply - casReserves
mkPoolRewardInfoCurry = mkPoolRewardInfo pr _R b (fromIntegral blocksMade) totalStake totalActiveStake
allPoolInfo = VMap.mapWithKey mkPoolRewardInfoCurry stakePoolSnapShots
blockProducingPoolInfo = VMap.mapMaybe (either (const Nothing) Just) allPoolInfo
```

`allPoolInfo` maps every registered pool to `Left StakeShare` (no blocks) or `Right PoolRewardInfo` (≥ 1 block). Only `blockProducingPoolInfo` produces actual rewards.

**Step 6 — `collectLRs` (leader rewards w/ Babbage hardfork guard):**
```haskell
collectLRs acc poolRI =
  let account = unAccountId $ spssAccountId $ poolPs poolRI
      packageLeaderReward = Set.singleton . leaderRewardToGeneral . poolLeaderReward
   in if hardforkBabbageForgoRewardPrefilter (pr ^. ppProtocolVersionL)
        || isAccountRegistered account accounts
      then Map.insertWith Set.union account (packageLeaderReward poolRI) acc
      else acc
```

- pv ≤ 6 (Shelley–Alonzo): leader reward only included if pool's reward account is currently registered in `accounts` (pre-Babbage gate).
- pv ≥ 7 (Babbage onward): registration check bypassed via `hardforkBabbageForgoRewardPrefilter` (ledger errata 17.2).

**Step 7 — `FreeVars` and pulser construction:**
```haskell
free = FreeVars
  (Map.keysSet (accounts ^. accountsMapL))      -- fvAddrsRew: registered stake credentials
  totalStake
  (pr ^. ppProtocolVersionL)
  blockProducingPoolInfo

pulser :: Pulser
pulser = RSLP pulseSize free (unActiveStake activeStake) (RewardAns Map.empty Map.empty)
```

`fvAddrsRew` is the set of currently registered staking credentials **at `startStep` time**. This is a snapshot — later registrations/deregistrations don't affect which credentials receive rewards.

### 3.2 `pulseStep` — Phase 2 (incremental)

```haskell
pulseStep (Complete r_)                  = pure (Complete r_, mempty)
pulseStep p@(Pulsing _ pulser) | done p  = completeStep p
pulseStep (Pulsing rewsnap pulser)       = do
  p2@(RSLP _ _ _ (RewardAns _ event)) <- pulseM pulser
  pure (Pulsing rewsnap p2, event)
```

`pulseM` takes `pulseSize` credentials from the front of the (sorted) VMap, applies `rewardStakePoolMember`, returns updated `RSLP` with those credentials removed and rewards folded into `RewardAns`.

### 3.3 `completeStep` / `completeRupd` — Phase 3

```haskell
let rs'    = Map.map Set.singleton rs_           -- member rewards as singleton sets
    rs''   = Map.unionWith Set.union rs' lrewards  -- merge with leader rewards
    deltaR2 = oldr <-> sumRewards protVer rs''   -- R - actually distributed
```

```haskell
pure (
  RewardUpdate
    { deltaT    = DeltaCoin deltaT1
    , deltaR    = invert (toDeltaCoin deltaR1) <> toDeltaCoin deltaR2
    , rs        = rs''
    , deltaF    = invert (toDeltaCoin feesSS)
    , nonMyopic = updateNonMyopic nm oldr newLikelihoods
    }
  , newevent
  )
```

`deltaR = deltaR2 - deltaR1`. If all rewards distributed, `deltaR2 = 0` and reserves decrease by exactly `deltaR1`.

**Wire-format sign-flip:** CBOR encoding inverts `deltaR` and `deltaF`:
```haskell
encCBOR (RewardUpdate dt dr rw df nm) =
  encCBOR dt <> encCBOR (invert dr) <> encCBOR rw <> encCBOR (invert df) <> encCBOR nm
```
Dugite must reproduce this inversion on encode/decode.

---

## 4. `mkPoolRewardInfo`: Pool Eligibility

```haskell
mkPoolRewardInfo pp r blocks blocksTotal (Coin totalStake) totalActiveStake
  stakePoolId stakePoolSnapShot =
  case Map.lookup stakePoolId (unBlocksMade blocks) of
    Nothing             -> Left $! StakeShare sigma
    Just numBlocksMade  ->
      let Coin maxP =
            if pledge <= selfDelegatedOwnersStake
              then maxPool' pp_a0 pp_nOpt r sigma poolRelativePledge
              else mempty   -- pledge not met → maxP = 0 → zero rewards
       in Right $! rewardInfo
```

### 4.1 Left vs Right
- **`Left StakeShare`**: pool produced zero blocks. No rewards. `sigma` returned for ranking only.
- **`Right PoolRewardInfo`**: pool produced ≥ 1 block.

### 4.2 Pledge check
- `pledge = spssPledge stakePoolSnapShot` (from registration cert)
- `selfDelegatedOwnersStake = spssSelfDelegatedOwnersStake stakePoolSnapShot`
  - = sum of stake for owners who are **also delegating to their own pool**
  - Distinct from "all owners" — owner delegating elsewhere doesn't count
- If `pledge > selfDelegatedOwnersStake`: `maxP = 0`, pool gets zero rewards even with blocks produced.

### 4.3 `maxPool'` formula

```haskell
maxPool' a0 nOpt r sigma pR = rationalToCoinViaFloor $ factor1 * factor2
  where
    z0      = 1 / nOpt
    sigma'  = min sigma z0
    p'      = min pR    z0
    factor1 = coinToRational r / (1 + a0)
    factor2 = sigma' + p' * a0 * factor3
    factor3 = (sigma' - p' * factor4) / z0
    factor4 = (z0 - sigma') / z0
```
- `nOpt` = k* (desired pool count). `z0 = 1/nOpt` is the saturation threshold.
- `sigma = poolTotalStake / totalStake` (using `totalStake = maxSupply - reserves`).
- `pR = pledge / totalStake`.
- `a0 = 0` → reduces to `r * min(sigma, z0)`.

### 4.4 Apparent performance

```haskell
appPerf = mkApparentPerformance pp_d sigmaA numBlocksMade blocksTotal
```
```haskell
mkApparentPerformance d_ sigma blocksN blocksTotal
  | sigma == 0                = 0
  | unboundRational d_ < 0.8  = beta / sigma
  | otherwise                 = 1
  where beta = toInteger blocksN % toInteger (max 1 blocksTotal)
```
- `sigmaA = poolTotalStake / totalActiveStake` (relative to **active** stake — different from sigma!)
- `beta = blocksMade / blocksTotal` (pool's block share)
- `appPerf = beta / sigmaA` (≈ 1 if pool produced its expected share)

```haskell
poolR = rationalToCoinViaFloor (appPerf * fromIntegral maxP)
```

---

## 5. `rewardOnePoolMember` and `collectLRs`

### 5.1 Member reward
```haskell
rewardOnePoolMember pv totalStake addrsRew rewardInfo hk (Coin c) =
  if prefilter && notPoolOwner (spssSelfDelegatedOwners (poolPs rewardInfo)) hk && r /= Coin 0
    then Just r
    else Nothing
  where
    prefilter  = hardforkBabbageForgoRewardPrefilter pv || hk `Set.member` addrsRew
    stakeShare = StakeShare $ c % unCoin totalStake
    r          = calcStakePoolMemberReward poolR spssCost spssMargin stakeShare sigma
```

Three conditions, all must hold:
1. **Prefilter:** pv ≥ 7 OR `hk ∈ addrsRew` (registered at `startStep` time).
2. **Not a pool owner:** owners receive only leader rewards, not member rewards.
3. **Non-zero reward.**

### 5.2 Member formula
```haskell
calcStakePoolMemberReward (Coin f) (Coin cost) margin (StakeShare t) (StakeShare sigma)
  | f <= cost = mempty                         -- pot too small → zero
  | otherwise =
      rationalToCoinViaFloor $
        fromIntegral (f - cost) * (1 - m) * t / sigma
  where m = unboundRational margin
```

### 5.3 Leader formula
```haskell
calcStakePoolOperatorReward f cost margin (StakeShare s) (StakeShare sigma)
  | f <= cost = f                              -- operator keeps everything
  | otherwise =
      cost <> rationalToCoinViaFloor
        (coinToRational (f <-> cost) * (m + (1 - m) * s / sigma))
  where m = unboundRational margin
```
- `s = selfDelegatedOwnersStake / totalStake`
- `sigma = poolTotalStake / totalStake`

### 5.4 `rewardStakePoolMember` (pulser inner function)

```haskell
rewardStakePoolMember freeVars inputAnswer cred swd =
  fromMaybe inputAnswer $ do
    let poolId = swdDelegation swd
    poolRI <- VMap.lookup poolId fvPoolRewardInfo            -- pool must be in blockProducingPoolInfo
    r      <- rewardOnePoolMember fvProtVer fvTotalStake fvAddrsRew poolRI cred
                (fromCompact $ unNonZero $ swdStake swd)
    let ans = Reward MemberReward poolId r
    pure $ RewardAns (Map.insert cred ans accum) (Map.insert cred (Set.singleton ans) recent)
```

**`fvPoolRewardInfo` contains only block-producing pools.** A credential whose pool is not in this map silently receives no reward.

### 5.5 `hardforkBabbageForgoRewardPrefilter`
```haskell
hardforkBabbageForgoRewardPrefilter pv = pvMajor pv > natVersion @6
```
- False for pv ≤ 6 (Shelley–Alonzo): `hk ∈ fvAddrsRew` prefilter applies. Credentials unregistered between `startStep` and `applyRUpd` get their share dropped.
- True for pv ≥ 7 (Babbage+): prefilter skipped. All go-snapshot credentials receive rewards; filtering happens only at `applyRUpd` time (unregistered → treasury).

Similarly `hardforkAllegraAggregatedRewards`:
```haskell
hardforkAllegraAggregatedRewards pv = pvMajor pv > natVersion @2
```
Before pv 3: if a credential earns from multiple sources (leader + member), only the first is delivered; rest silently dropped. From pv ≥ 3: all aggregated.

---

## 6. `applyRUpd`: Applying the Reward Update

### 6.1 Entry point (in NEWEPOCH)
```haskell
newEpochTransition = do
  ...
  es' <- case ru of
    SNothing               -> pure es
    SJust p@(Pulsing _ _)  -> ... completeRupd ... updateRewards es eNo ans
    SJust (Complete ru')   -> updateRewards es eNo ru'
  es''  <- trans @(EraRule "MIR"   era) $ TRC ((), es', ())
  es''' <- trans @(EraRule "EPOCH" era) $ TRC ((), es'', eNo)
```

`updateRewards` runs `applyRUpdFiltered` and emits trace events. Internal assertion:
```haskell
let totRs = sumRewards (es ^. prevPParamsEpochStateL . ppProtocolVersionL) rs_
 in assert (Val.isZero (dt <> (dr <> toDeltaCoin totRs <> df))) (pure ())
```
Verifies: **deltaT + deltaR + sum(rs) + deltaF = 0** (debug-only).

### 6.2 `applyRUpdFiltered` — exact mutation order

```haskell
applyRUpdFiltered ru es@(EpochState as ls ss _nm) = (epochStateAns, filteredRewards)
  where
    filteredRewards@FilteredRewards { frRegistered, frTotalUnregistered }
      = filterAllRewards' (rs ru) prevProVer dState
    registeredAggregated = aggregateCompactRewards prevProVer frRegistered
    as' = as
      { casTreasury = addDeltaCoin (casTreasury as) (deltaT ru) <> frTotalUnregistered
      , casReserves = addDeltaCoin (casReserves as) (deltaR ru)
      }
    ls' = ls
      & lsUTxOStateL . utxosFeesL %~ (`addDeltaCoin` deltaF ru)
      & lsCertStateL . certDStateL . accountsL %~ addToBalanceAccounts registeredAggregated
    nm' = nonMyopic ru
```

**Five simultaneous mutations:**
1. **Treasury**: `casTreasury += deltaT + frTotalUnregistered`
   - `deltaT = DeltaCoin deltaT1` (= τ × rPot, always positive)
   - `frTotalUnregistered` = sum of rewards for credentials **not currently registered** (orphaned → treasury, not back to reserves)
2. **Reserves**: `casReserves += deltaR`
   - `deltaR = deltaR2 - deltaR1`. Net negative if rewards actually distributed.
   - `deltaR2` returns undistributed amounts (rounding/non-participation) to reserves.
3. **Fees**: `utxosFeesL += deltaF`
   - `deltaF = invert (toDeltaCoin feesSS)` = negative of go-snapshot fees.
   - Decrements the on-chain fee accumulator by the amount swept into the reward pot.
   - Fees collected during the current epoch (after go was snapshotted) unaffected.
4. **Reward accounts**: each registered credential's balance += its aggregated reward.
5. **NonMyopic**: updated with new pool likelihoods for ranking.

### 6.3 `filterAllRewards'` — partition by registration

```haskell
filterAllRewards' rewards protVer dState =
  FilteredRewards registered shelleyIgnored unregistered totalUnregistered
  where
    (registeredRewardsUpdate, unregisteredRewardsUpdate) =
      Map.partitionWithKey
        (\cred _ -> isAccountRegistered cred (dState ^. accountsL)) rewards
    totalUnregistered = fold $ aggregateRewards protVer unregisteredRewardsUpdate
    unregistered      = Map.keysSet unregisteredRewardsUpdate
    (registered, shelleyIgnored) = filterRewards protVer registeredRewardsUpdate
```

- Partition by current registration status (at `applyRUpd` time, not `startStep` time).
- `shelleyIgnored`: dropped due to Shelley-era multi-source limitation (pv ≤ 2 only).
- `frTotalUnregistered`: aggregated sum of unregistered rewards → routes to treasury.

---

## 7. Conservation Laws

### 7.1 Six-pot invariant (`AdaPots`)

```
totalAda = treasury + reserves + rewards + utxo + fees + obligations
```

Where:
- `treasury` = `casTreasury`
- `reserves` = `casReserves`
- `rewards` = `sumBalancesAccounts` (all DState account balances)
- `utxo` = `sumCoinUTxO utxo` (lovelace-only)
- `fees` = `utxosFeesL`
- `obligations` = deposits for stake keys + pools + governance proposals/votes

Equals `maxLovelaceSupply` always (minus any unclaimed Shelley-era stash).

### 7.2 `RewardUpdate` internal conservation
```
deltaT + deltaR + sum(aggregateRewards pv rs) + deltaF = 0
```
Algebra:
- `deltaT = +deltaT1`
- `deltaR = -deltaR1 + deltaR2`
- `sum(rs) = _R - deltaR2`
- `deltaF = -feesSS`
- `rPot = feesSS + deltaR1`, `_R = rPot - deltaT1`

Sum: `deltaT1 + (-deltaR1 + deltaR2) + (_R - deltaR2) + (-feesSS) = deltaT1 + _R - deltaR1 - feesSS = rPot - deltaR1 - feesSS = 0` ✓

### 7.3 After `applyRUpd`
| Pot | Change |
|---|---|
| treasury | +deltaT1 + frTotalUnregistered |
| reserves | +(deltaR2 - deltaR1) |
| rewards | +sum(frRegistered) - frShelleyIgnored |
| utxo | 0 |
| fees | -feesSS |
| obligations | 0 |

Six-pot total invariant across `applyRUpd`.

---

## 8. Snapshot Lifecycle Invariant

### 8.1 The go = mark(N-2) property

At boundary N→(N+1), SNAP executes:
```
ssStakeMark_{N+1} := snapshot_of_current_instantStake
ssStakeSet_{N+1}  := ssStakeMark_{N}
ssStakeGo_{N+1}   := ssStakeSet_{N} = ssStakeMark_{N-1}
```

`ssStakeGo` at N→(N+1) = `ssStakeMark` at (N-1)→N = snapshot from end of epoch N-1.

When `startStep` for epoch N+1 runs (during epoch N+1, slots 4k/f to 8k/f), it uses `ssStakeGo(N+1)` = snapshot from end of epoch N-1.

- Active stake used to compute rewards for epoch N+1 = stake state at end of epoch N-1.
- Pool parameters (`StakePoolSnapShot`) from same go snapshot.
- Pledge values + owner sets from `pstate.psStakePools` at snapshot time.

### 8.2 `StakePoolSnapShot` fields

| Field | Source | Notes |
|---|---|---|
| `spssStake` | Sum of `ActiveStake` for delegators | `sumCredentialsCompactActiveStake activeStake spsDelegators` |
| `spssStakeRatio` | `spssStake / totalActiveStake` | Precomputed |
| `spssSelfDelegatedOwners` | `spsOwners ∩ spsDelegators` | Owners delegating to own pool |
| `spssSelfDelegatedOwnersStake` | Sum active stake for above | **Used for pledge check** |
| `spssVrf` | `spsVrf` | VRF verkey hash |
| `spssPledge` | `spsPledge` | Declared pledge |
| `spssCost` | `spsCost` | Min fixed cost |
| `spssMargin` | `spsMargin` | Operator margin |
| `spssNumDelegators` | `Set.size spsDelegators` | Excludes 0-delegator pools from PoolDistr |
| `spssAccountId` | `spsAccountId` | Pool reward account (staking credential) |

**Pledge check uses `spssSelfDelegatedOwnersStake`** — owners delegating to other pools don't count.

### 8.3 `nesPd` — pool distribution for leader check

```haskell
let pd' = ssStakeMarkPoolDistr (esSnapshots es)
```

`nesPd` at N→(N+1) = `calculatePoolDistr(ssStakeMark at N→(N+1))`. Distribution based on freshly computed mark. Used for VRF leader check during epoch N+1.

**Reward calc uses `ssStakeGo` (2 epochs old); leader election uses `nesPd` from `ssStakeMark` (current).** Independent: pool can have 0 active stake in go (no rewards) but stake in mark (still eligible to forge).

### 8.4 `ssFee` lifecycle

- `ssFee` set at boundary N→(N+1) to `utxosFeesL` at that moment.
- `ssFee` of go snapshot at boundary N→(N+1) = `ssFee` of set at (N-1)→N = `ssFee` of mark at (N-2)→(N-1) = `utxosFeesL` at (N-2)→(N-1).
- `startStep` uses `ssFee ss` where `ss = ssStakeGo`: `rPot = fees_from_(N-2)_boundary + deltaR1`.
- **Two-epoch lag matches stake distribution lag.**

---

## 9. Full Epoch Timeline

For epoch N (slots `[first_N, first_{N+1})`):

| When | Event | Operation |
|---|---|---|
| Slot `first_N + 4k/f + 1` (first block) | RUPD JustRight | `startStep`: go snapshot, compute deltaR1/deltaT1/poolInfo, create RSLP pulser |
| Slots `first_N + 4k/f` to `+8k/f` | Each block | `pulseStep`: process `ceil(numCreds/4k)` member rewards per block |
| Slot `first_N + 8k/f + 1` | RUPD TooLate | `completeStep`: drain remaining, compute deltaR2, package `RewardUpdate` |
| Slot `first_{N+1}` (first block of N+1) | NEWEPOCH | (1) `applyRUpd` applies N's update; (2) MIR; (3) EPOCH → SNAP, POOLREAP, UPEC; `nesPd := ssStakeMarkPoolDistr` |

Reward update computed during epoch N is applied at the **start of epoch N+1**, before SNAP runs.
- `prevPParams` used in `startStep` = PParams active during epoch N-1 (became `prevPParams` at N-1→N boundary).
- `ssFee` used = go-snapshot fees from N-2→N-1 boundary.

---

## 10. Critical Notes for Dugite

1. **`prevPParams` in `startStep`**: must pull `prev_pparams` (NOT `cur_pparams`) for ρ, τ, d, a0, nOpt, protocolVersion. Lens: `prevPParamsEpochStateL` reads from `govState.prevPParams`.

2. **`ssFee` source**: go-snapshot `ssFee` is two-rotations old (from N-2→N-1 boundary). NOT current or set fees.

3. **`fvAddrsRew`**: `Map.keysSet (accounts ^. accountsMapL)` at `startStep` time. Large snapshot — avoid recomputing during pulsing.

4. **Pledge check uses `spssSelfDelegatedOwnersStake`**: NOT sum of all owners — only owners delegating to own pool. Pool whose owners delegate elsewhere has `selfDelegatedOwnersStake = 0` and fails pledge even with declared pledge = 0.

5. **`totalStake = circulation`**: `maxSupply - casReserves` at `startStep` time. NOT sum of UTxO or active stake.

6. **`sigmaA` vs `sigma`**:
   - `sigma  = poolStake / totalStake`
   - `sigmaA = poolStake / totalActiveStake`
   - Apparent performance uses `sigmaA`; `maxPool'` uses `sigma`.

7. **Rounding**: all coin values via `rationalToCoinViaFloor` (floor). Never round-to-nearest. Floating-point will diverge.

8. **`deltaR2` → reserves**: undistributed rewards (`_R - sum(rs)`) return to **reserves** (`deltaR`), NOT treasury. Only unregistered rewards go to treasury via `frTotalUnregistered`.

9. **CBOR sign flip**: `RewardUpdate` serializes `deltaR` and `deltaF` with inverted sign. In-memory `deltaR` is negative when reserves decrease.

10. **Shelley-era dedup**: pv ≤ 2 `filterRewards` drops all but first reward per credential. pv ≥ 3 all aggregated. Conway (pv ≥ 10) always aggregated path.

11. **Babbage prefilter bypass**: pv ≥ 7 skips `hk ∈ fvAddrsRew` prefilter at calculation time. Unregistered routing happens only at `applyRUpd` (via `frTotalUnregistered`).
