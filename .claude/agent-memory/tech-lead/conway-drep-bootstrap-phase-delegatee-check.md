---
name: conway-drep-bootstrap-phase-delegatee-check
description: DelegateeDRepNotRegisteredDELEG is SKIPPED at PV9 (Conway bootstrap); only fires at PV>=10
metadata:
  type: project
---

Haskell `cardano-ledger` Conway DELEG rule `Deleg.hs` `checkDRepRegistered` has an explicit gate:

```haskell
unless (hardforkConwayBootstrapPhase (pp ^. ppProtocolVersionL)) $
  targetDRep `Map.member` dReps ?! injectFailure (DelegateeDRepNotRegisteredDELEG targetDRep)
```

Where `hardforkConwayBootstrapPhase pv = pvMajor pv == 9`.

**Why:** During Conway bootstrap (PV9), many users self-registered as DRep and self-delegated in the same tx. The common pattern is cert[0]=VoteDelegation (to the DRep being registered), cert[1]=RegDRep (registering the DRep). At PV9 this pattern is explicitly allowed.

**Bug fixed:** dugite used `params.protocol_version_major >= 9` as the gate for this check, but Haskell only activates it at `>= 10`. This caused 26 on-chain mainnet txs (epoch 507+) to be falsely rejected.

**Fix location:** `crates/dugite-ledger/src/validation/mod.rs:3184` — change `>= 9` to `>= 10`.

**Test to update:** `tests.rs:10339` `test_vote_deleg_to_unregistered_drep_rejected` uses PV9 and must be updated to PV10. New PV9 regression tests needed.

**How to apply:** Any time VoteDelegation validation is touched, remember this bootstrap exemption. Also remember the `new_dreps` forward-scan (same block handles RegDRep-before-VoteDelegation at PV10+). At PV9 the entire block is skipped.
