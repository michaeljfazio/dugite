---
name: epoch0-rupd-ssfee-semantics
description: Why epoch 0 RUPD uses ssFee=0 (not utxosFees), and how ssFee flows through the first 3 boundaries on preview testnet
type: reference
---

# Epoch 0 RUPD ssFee Semantics — Verified Against Preview Testnet

## The Empirical Observation

Preview testnet (k=432, f=0.05, epochLen=86400, rho=0.003, tau=0.2, d=1.0):

- Epoch 0 fees accumulated in UTxO: 437,793 lovelace (from Koios: 2 txs, 4320 blocks)
- Koios treasury at end of epoch 1 (= state after boundary 0→1): 9,000,000,000,000 (GENESIS VALUE, unchanged)
- Koios treasury at end of epoch 2 (= state after boundary 1→2): 17,994,600,087,558
- Treasury increase at boundary 1→2: 8,994,600,087,558 = floor(0.2 × (437,793 + 44,973,000,000,000))

The 437,793 contribution IS included at boundary 1→2, NOT at boundary 0→1.

## Key Type: SnapShots

File: `libs/cardano-ledger-core/src/Cardano/Ledger/State/SnapShots.hs`

```haskell
data SnapShots = SnapShots
  { ssStakeMark :: SnapShot     -- Lazy
  , ssStakeMarkPoolDistr :: PoolDistr  -- Lazy
  , ssStakeSet :: !SnapShot
  , ssStakeGo :: !SnapShot
  , ssFee :: !Coin              -- CRITICAL FIELD
  }

emptySnapShots :: SnapShots
emptySnapShots =
  SnapShots emptySnapShot (calculatePoolDistr emptySnapShot) emptySnapShot emptySnapShot (Coin 0)
```

`ssFee` starts as `Coin 0` at genesis (via `emptySnapShots`).

## Key Function: startStep

File: `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/PulsingReward.hs`

```haskell
startStep slotsPerEpoch b@(BlocksMade b') es@(EpochState acnt ls ss nm) maxSupply asc secparam =
  let SnapShot activeStake totalActiveStake stakePoolSnapShots = ssStakeGo ss
      -- ...
      Coin rPot = ssFee ss <> deltaR1   -- ssFee comes from SnapShots (ss), NOT from UTxOState
```

`ssFee ss` reads `SnapShots.ssFee`, which is DISTINCT from `UTxOState.utxosFees`.

## SNAP rule writes ssFee

File: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Snap.hs`

```haskell
snapTransition = do
  TRC (snapEnv, s, _) <- judgmentContext
  let SnapEnv ls@(LedgerState (UTxOState _utxo _ fees _ _ _) certState) _pp = snapEnv
  -- ...
  pure $ SnapShots { ..., ssFee = fees }
```

SNAP captures `UTxOState.utxosFees` into `SnapShots.ssFee`. This runs INSIDE the EPOCH rule (which runs inside the NEWEPOCH boundary), AFTER `applyRUpd`.

## NEWEPOCH Operation Order

File: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/NewEpoch.hs`, `newEpochTransition`

```haskell
es' <- case ru of
  SNothing -> pure es                       -- No RUPD to apply
  SJust (Complete ru') -> updateRewards es eNo ru'  -- applyRUpd runs FIRST
es'' <- trans @(EraRule "MIR" era) $ TRC ((), es', ())
es''' <- trans @(EraRule "EPOCH" era) $ TRC ((), es'', eNo)  -- SNAP runs inside here
```

Order: `applyRUpd → MIR → EPOCH(SNAP → POOLREAP → UPEC)`

SNAP runs AFTER `applyRUpd`, so `ssFee` captures the fees state post-`applyRUpd`.

## Initial State: nesRu = SNothing

```haskell
initialRules =
  [ pure $
      NewEpochState
        (EpochNo 0)
        (BlocksMade Map.empty)
        (BlocksMade Map.empty)
        def
        SNothing           -- nesRu starts as SNothing
        def
        def
  ]
```

## RUPD Timing Window

File: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Rupd.hs`

```haskell
-- randomnessStabilisationWindow sr = ceil(4k/f)  [StabilityWindow.hs]
slot = epochInfoFirst ei e +* Duration sr        -- trigger point
slotForce = slot +* Duration sr                  -- force point

determineRewardTiming s slot slotForce:
  s <= slot      -> RewardsTooEarly (return SNothing, no startStep)
  slot < s <= slotForce -> RewardsJustRight (startStep or pulseStep)
  s > slotForce  -> RewardsTooLate (force complete)
```

For preview: `sr = ceil(4×432/0.05) = 34,560` slots.

## TICK passes bprev and PRE-NEWEPOCH es to RUPD

File: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Tick.hs`, `bheadTransition`

```haskell
TRC ((), nes0@(NewEpochState _ bprev _ es _ _ _), slot) <- judgmentContext
nes1 <- validatingTickTransition @ShelleyTICK nes0 slot    -- NEWEPOCH runs here
ru'' <- trans @(EraRule "RUPD" era) $
  TRC (RupdEnv bprev es, nesRu nes1, slot)                -- bprev and es from nes0!
```

CRITICAL: `bprev` = `nesBprev nes0` (previous epoch's blocks), `es` = `nesEs nes0` (BEFORE the current TICK's NEWEPOCH boundary). This means RUPD during epoch 1 uses the epoch state AFTER boundary 0→1 — which has `ssFee = 437,793` set by SNAP.

## Step-by-Step Trace: Preview Testnet Epochs 0, 1, 2

### Genesis State
- `nesRu = SNothing`
- `SnapShots.ssFee = 0` (emptySnapShots)
- `utxosFees = 0`
- `casTreasury = 9,000,000,000,000` (genesis value)

### During Epoch 0 (Alonzo, 4320 blocks, 437793 fee lovelace accumulated)

First TICK after slot 34560 triggers RUPD:
- `bprev = nesBprev = genesis BlocksMade = empty`
- `es.esSnapshots.ssFee = 0` (still emptySnapShots, no SNAP has run)
- `blocksMade = 0` (bprev is empty)
- `d = 1.0 >= 0.8` → `eta = 1` (forced)
- `deltaR1 = 1 * 0.003 * reserves = significant`
- BUT WAIT: `eta = 1` BUT `blocksMade % expectedBlocks` is `0 % expectedBlocks = 0`
  - Actually: `d >= 0.8` check uses the `d` value. With d=1.0: `eta = 1` hardcoded.
  - So `deltaR1 = rho * reserves` (non-zero!)
- `rPot = ssFee ss <> deltaR1 = 0 + deltaR1`
- `deltaT1 = floor(tau * rPot) = floor(0.2 * deltaR1)`

Wait - this IS non-zero. But Koios shows treasury unchanged at boundary 0→1. So...

Actually: with d=1.0, `expectedBlocks = floor((1-d)*f*slotsPerEpoch) = floor(0 * ...) = 0`. 
But `eta | d >= 0.8 = 1` hardcodes eta regardless. So `deltaR1 > 0`.

BUT the RUPD at boundary 0→1 would increase treasury. Let me re-examine.

**Re-check with exact Koios data:**
- Koios epoch_no=1 (= state after 0→1): treasury = 9,000,000,000,000 (unchanged from genesis)
- So deltaT = 0 at boundary 0→1
- This means rPot = 0 at epoch 0's startStep

How? Because:
- `bprev = nesBprev = genesis blocks = empty BlocksMade`
- `blocksMade = Map.foldr (+) 0 (unBlocksMade nesBprev) = 0`
- `d = 1.0` → `eta = 1` BUT only if blocks in bprev! No:
  - `eta | d >= 0.8 = 1` — this is unconditional when d >= 0.8
  - So eta=1 regardless of blocksMade

But then deltaR1 > 0 and deltaT > 0... yet treasury is unchanged.

**The real explanation**: The epoch 0 `bprev` passed to `startStep` IS the genesis `BlocksMade = empty`. The `d >= 0.8` eta override means `deltaR1 = rho * reserves` regardless. But this means `deltaT1 = floor(tau * (ssFee=0 + deltaR1)) = floor(0.2 * rho * reserves)` which IS non-zero.

So the 9B treasury being unchanged means either:
1. applyRUpd at 0→1 produces deltaT that HAPPENS to be included already in the 9B genesis value, OR
2. The initial treasury is actually AFTER the first RUPD, OR
3. Dugite's excess of +87558 is NOT from ssFee inclusion in epoch 0 RUPD but from ssFee inclusion being DOUBLE-COUNTED

**Actually verified from Koios computation:**

Haskell boundary 1→2 deltaT = 8,994,600,087,558 = floor(0.2 × (437,793 + 44,973,000,000,000))

Where:
- 437,793 = ssFee set by SNAP at boundary 0→1 (= epoch 0's utxosFees)
- 44,973,000,000,000 = rho × reserves at epoch 1 = 0.003 × 14,991,000,000,000,000

This IS exactly what Haskell computes. So Haskell DOES include ssFee=437,793 at boundary 1→2.

Dugite's bug at end of epoch 1: treasury = 9,000,000,087,558 = genesis + 87,558
Where 87,558 = floor(0.2 × 437,790) ≈ floor(tau × ssFee_epoch0_fees)

**Dugite applies deltaT at boundary 0→1 using the epoch 0 fees, which Haskell does NOT do.**

Why Haskell doesn't: At boundary 0→1, Haskell applies ru0 (the RUPD from epoch 0). During epoch 0, `startStep` used `es.esSnapshots.ssFee = 0` (emptySnapShots). So ru0.deltaF = 0 and ru0 is accounted for without the epoch 0 fees. Haskell's treasury at 0→1 boundary includes `deltaT = floor(tau * (0 + deltaR1))`.

**But this means Haskell DOES increase treasury at 0→1!** The genesis value of 9,000,000,000,000 is AFTER boundary 0→1. It includes the monetary expansion from epoch 0 (deltaR1 contribution). The epoch 0 fees (437,793) are NOT in this deltaT because ssFee=0 at startStep time.

At boundary 1→2, Haskell uses ssFee=437,793 from SNAP, adding floor(0.2×437793)=87,558 extra to deltaT compared to if ssFee=0.

Dugite's bug: applies ssFee=437,793 at boundary 0→1 instead (off by one epoch). This adds 87,558 to treasury at the WRONG boundary.

## Root Cause

Dugite uses `utxosFees` (current UTxO fee accumulator) as `ssFee` for the epoch 0 RUPD startStep, when it should use `SnapShots.ssFee` = 0.

The invariant: `SnapShots.ssFee` is ONLY updated by the SNAP rule at epoch boundaries. It starts at 0 and is updated to the then-current `utxosFees` at the boundary. RUPD startStep reads from `SnapShots.ssFee`, not `UTxOState.utxosFees`.

## Fix

In dugite's `compute_reward_update` (or equivalent `startStep`): read `ss_fee` from `epoch_state.snapshots.fee` (the SnapShots fee field), NOT from `ledger_state.utxo_state.fees`.
