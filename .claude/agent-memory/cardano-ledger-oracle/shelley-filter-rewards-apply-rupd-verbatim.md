---
name: shelley-filter-rewards-apply-rupd-verbatim
description: Verbatim-verified (2026-08-10 @ adcb341f) Shelley pv<=2 reward filter + applyRUpdFiltered semantics — Set.deleteFindMin keeps Leader-first min, totalUnregistered = aggregated (FILTERED) unreg sum to treasury, deltaR2 uses FROZEN rewProtocolVersion, fvAddrsRew freeze + collectLRs gate, and the prevPParams pv timeline (last filtered boundary = one epoch AFTER the HF epoch starts).
metadata:
  type: reference
---

All verified verbatim against IntersectMBO/cardano-ledger master
@ `adcb341f236fd224f60577a79ffeb5fb138f051f` (2026-08-10).

## filterRewards / aggregateRewards / sumRewards (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs`)

```haskell
filterRewards pv rewards =
  if hardforkAllegraAggregatedRewards pv          -- pvMajor pv > 2 (Era.hs:232)
    then (rewards, Map.empty)
    else
      let mp = Map.map Set.deleteFindMin rewards
       in (Map.map (Set.singleton . fst) mp, Map.filter (not . Set.null) $ Map.map snd mp)

aggregateRewards pv rewards = Map.map (foldMap' rewardAmount) $ fst $ filterRewards pv rewards
sumRewards protocolVersion rs = fold $ aggregateRewards protocolVersion rs
```

- KEPT element at pv<=2 = `Set.deleteFindMin` = the MIN under `Ord Reward`
  (`libs/cardano-ledger-core/src/Cardano/Ledger/Rewards.hs`):
  `LeaderReward < MemberReward`, tie by ascending pool KeyHash, AMOUNT IGNORED.
  (The instance comment mentions `Set.findMax` — stale/confusing; the code path
  is `deleteFindMin`.) dugite's sort key `(is_member, pool_id)` + `.first()`
  matches exactly, and 20+ mainnet epochs of byte-exact pots with up to
  ~2.5e11 lovelace/epoch of dropped extras (epochs 223-227) prove it in production.

## completeRupd deltaR2 uses the FROZEN pv (`PulsingReward.hs:260-278`)

```haskell
RewardAns rs_ events <- completeM pulser
let rs'  = Map.map Set.singleton rs_             -- members: at most ONE per cred
let rs'' = Map.unionWith Set.union rs' lrewards  -- + leader rewards (Set)
let deltaR2 = oldr <-> sumRewards protVer rs''   -- protVer = rewProtocolVersion FROZEN at startStep
```

So at pv<=2 the dropped (non-min) rewards are inside deltaR2 -> RESERVES.

## applyRUpdFiltered / filterAllRewards' (`.../LedgerState/IncrementalStake.hs:95-169`)

```haskell
prevProVer = es ^. prevPParamsEpochStateL . ppProtocolVersionL   -- APPLY-time prevPParams
(registeredRewardsUpdate, unregisteredRewardsUpdate) =
  Map.partitionWithKey (\cred _ -> isAccountRegistered cred (dState ^. accountsL)) rewards
totalUnregistered = fold $ aggregateRewards protVer unregisteredRewardsUpdate  -- FILTERED (min at pv<=2)
(registered, shelleyIgnored) = filterRewards protVer registeredRewardsUpdate
casTreasury' = addDeltaCoin casTreasury (deltaT ru) <> frTotalUnregistered
casReserves' = addDeltaCoin casReserves (deltaR ru)
```

Partition FIRST, filter SECOND — but since both the filter and the partition are
per-credential, this commutes with dugite's filter-at-compute-then-partition
order. PROVEN observationally equivalent given (1) identical per-cred reward-set
contents, (2) identical min selection, (3) identical registration partition,
(4) frozen pv == apply pv. An unregistered multi-reward cred at pv<=2:
min -> treasury, extras -> deltaR2 -> reserves, on BOTH orderings.

## fvAddrsRew + leader gate (`PulsingReward.hs`)

- `fvAddrsRew = Map.keysSet (accounts ^. accountsMapL)` frozen in `startStep`
  (line 201) — the registered accounts at the tick of the FIRST block strictly
  after `first_slot + 4k/f`, state BEFORE that block's certs.
- Member prefilter (`Rewards.hs:315`): `prefilter = hardforkBabbageForgoRewardPrefilter pv
  || hk `Set.member` addrsRew` — pv<=6 drops the member reward ENTIRELY (never
  created; stays in pot -> deltaR2 -> reserves).
- Leader gate `collectLRs` (`PulsingReward.hs:177-183`): included iff
  `hardforkBabbageForgoRewardPrefilter pv || isAccountRegistered account accounts`
  — same startStep-time accounts; frozen into `RewardSnapShot.rewLeaders`.

## The prevPParams pv timeline (easy to get off by one)

`startStep` during epoch e reads `pr = es ^. prevPParamsEpochStateL` = params in
force during epoch e-1. So for the Allegra HF (mainnet PV3 current from epoch 236):
the RUPD applied at boundary 236->237 (frozen during epoch 236) STILL runs the
pv2 single-reward filter — the LAST filtered boundary is 236->237, not 235->236.
First aggregated boundary: 237->238. Empirically confirmed by the mainnet
cstreamer dumps: the extras gap (unfiltered-vs-filtered totalDistributed)
collapses from ~1.1e9 at the 236 dump to ~-391 at the 237 dump.

See [[project_1074_first_pulse_prefilter_hole]] for the dugite divergence this
was verified for, and [[reward-calc-floor-chain-and-sigma-vs-sigmaA]] for the
rest of the reward chain.
