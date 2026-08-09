---
name: shelley-reward-aggregation-and-unregistered-routing
description: applyRUpdFiltered pot routing (treasury = deltaT + frTotalUnregistered), PV2 filterRewards Set.deleteFindMin keeps the LEADER reward, and every PV consult in the reward pipeline reads prevPParams — so Shelley semantics govern one boundary PAST the Allegra HF
type: reference
---

# Shelley reward aggregation + unregistered-reward routing (verified cardano-ledger-shelley-1.17.0.0, commit faa7a9dc347697b11d4da5b7818b1731e11aeeef; master byte-identical on all load-bearing functions as of 2026-08-09)

## applyRUpdFiltered (eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/IncrementalStake.hs)
- treasury: `casTreasury' = addDeltaCoin (casTreasury as) (deltaT ru) <> frTotalUnregistered`
- reserves: `casReserves' = addDeltaCoin (casReserves as) (deltaR ru)` — NOTHING else touches reserves at application
- fees: `utxosFeesL %~ (addDeltaCoin deltaF)`; accounts: `addToBalanceAccounts registeredAggregated`
- PV for BOTH the registered/unregistered partition aggregation: `prevProVer = es ^. prevPParamsEpochStateL . ppProtocolVersionL` (application-time state, BEFORE the same boundary's EPOCH rotates prevPP)
- registration predicate: `Map.partitionWithKey (\cred _ -> isAccountRegistered cred (dState ^. accountsL))` — DState accounts AT APPLICATION TIME (pre-MIR, pre-EPOCH: NEWEPOCH order is updateRewards -> MIR -> EPOCH)
- `totalUnregistered = fold $ aggregateRewards protVer unregisteredRewardsUpdate` — under PV<=2 only the Ord-min element per credential is counted; the ignored remainder NEVER leaves reserves (deltaR2 in completeRupd uses the same `sumRewards protVer`, protVer frozen at startStep). So a multi-element Set on an UNREGISTERED credential produces the exactly-compensating treasury(+X)/reserves(-X) signature between PV2 and PV3 semantics.

## filterRewards (eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs)
```haskell
filterRewards pv rewards =
  if hardforkAllegraAggregatedRewards pv
    then (rewards, Map.empty)
    else
      let mp = Map.map Set.deleteFindMin rewards
       in (Map.map (Set.singleton . fst) mp, Map.filter (not . Set.null) $ Map.map snd mp)
```
`Ord Reward` (libs/cardano-ledger-core/src/Cardano/Ledger/Rewards.hs) is HAND-WRITTEN: LeaderReward < MemberReward, tie broken by pool keyhash ONLY (amounts never compared). So Set.deleteFindMin keeps the LEADER reward when a credential has both; among several leader rewards the numerically smallest pool keyhash wins. (The instance comment mentions Set.findMax — stale wording; the operative code keeps the minimum.)

## PV gates (eras/shelley/impl/src/Cardano/Ledger/Shelley/Era.hs)
- `hardforkAllegraAggregatedRewards pv = pvMajor pv > natVersion @2` (aggregation on at PV3+)
- `hardforkBabbageForgoRewardPrefilter pv = pvMajor pv > natVersion @6` (prefilter dropped at PV7+; comment cites "Shelley Ledger Errata 17.2")
- Prefilter (PV<=6): member — `rewardOnePoolMember`: `prefilter = hardforkBabbageForgoRewardPrefilter pp || hk \`Set.member\` addrsRew` where `fvAddrsRew = Map.keysSet (accounts ^. accountsMapL)` captured at startStep; leader — `collectLRs` in startStep gates on `isAccountRegistered account accounts` at startStep. Dropped-by-prefilter rewards never enter `rs` -> stay in reserves via deltaR2.

## prevPParams timing (the trap)
EVERY PV consult in the pipeline reads `prevPParamsEpochStateL`: startStep (also rho/tau/d/a0/nOpt VALUES), completeRupd via frozen `rewProtocolVersion`, applyRUpdFiltered, and updateRewards' conservation assert. EPOCH sets `prevPP := curPP-of-ending-epoch` AFTER applyRUpd at each boundary, so during epoch N prevPP = epoch N-1's pparams. Mainnet Allegra (PV3 at epoch 236): the rupd computed during epoch 236 AND applied at 236->237 still runs PV2 semantics end-to-end. First fully-PV3 reward flow is applied at 237->238.

## NEWEPOCH conservation assert (Rules/NewEpoch.hs updateRewards)
`assert (Val.isZero (dt <> (dr <> toDeltaCoin totRs <> df))))` with `totRs = sumRewards prevPV rs` — deltaT + deltaR + aggregated-total + deltaF = 0.

## Shelley->Allegra translation (eras/allegra/impl/src/Cardano/Ledger/Allegra/Translation.hs)
`nesEs = translateEra' ctxt $ returnRedeemAddrsToReserves $ nesEs nes`; `nesRu = nesRu nes` (in-flight pulser carried unchanged). `returnRedeemAddrsToReserves` (LedgerState/NewEpochState.hs) partitions UTxO on `isBootstrapRedeemer`, adds their sum to casReserves, deletes them from UTxO.

## Errata (eras/shelley/formal-spec/errata.tex, same tag)
- "Reward aggregation": intended sum-of-all; "a mistake caused reward accounts to receive at most one of them". "corrected at the Allegra hard fork. There were sixty-four stake addresses that were affected, each of which was reimbursed for the exact amount lost using a MIR certificate" — reimbursement txs a01b9fe1... (unregistered, 4 addrs) and 8cab8049... (registered).
- "Byron redeem addresses": removed from UTxO at Allegra HF, Ada returned to RESERVES.
- "Total stake calculation": createRUpd uses CURRENT reserves for totalStake (should be previous epoch's) — permanent behavior, never fixed.
- "Active stake registrations" (=17.2 in built PDF): prefilter; fixed Babbage PV7.
