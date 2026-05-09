---
name: applyChainTick Forge Mutations
description: Exact fields mutated by TICK/NEWEPOCH visible to the forge path, with epoch-boundary vs. intra-epoch behaviour, minimum-correct forecast requirements
type: reference
---

## Source Files

- Conway NEWEPOCH: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/NewEpoch.hs` (`newEpochTransition`)
- Conway EPOCH: `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Epoch.hs` (`epochTransition`)
- Shelley SNAP: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Snap.hs` (`snapTransition`)
- Shelley TICK/bheadTransition: `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Tick.hs`
- Conway TICKF (forecast): `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Tickf.hs`
- applyRUpdFiltered: `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/IncrementalStake.hs`
- FuturePParams / nextEpochPParams: `libs/cardano-ledger-core/src/Cardano/Ledger/State/Governance.hs`
- consensus applyChainTick: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/Ledger.hs`
- Praos.LedgerView: `ouroboros-consensus-protocol/src/ouroboros-consensus-protocol/Ouroboros/Consensus/Protocol/Praos/Views.hs`
- protocolLedgerView (what forge reads): `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/SupportsProtocol.hs`
- Mempool pparams reads: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/Mempool.hs`
- getPParams definition: `ouroboros-consensus-cardano/src/shelley/Ouroboros/Consensus/Shelley/Ledger/Ledger.hs` line 801

## What applyChainTick (TICK) Does

`applyChainTick OmitLedgerEvents cfg slotNo st` calls `SL.applyTickNoEvents globals shelleyLedgerState slotNo`.

In the ledger, this runs `bheadTransition`:
1. `solidifyNextEpochPParams nes0 slot` — if slot >= point-of-no-return (2 stability windows before epoch end), converts `PotentialPParamsUpdate(Just pp)` → `DefinitePParamsUpdate pp` in `cgsFuturePParams`. Pure structural no-op otherwise.
2. `validatingTickTransition` → `trans NEWEPOCH` at epoch boundary (or no-op if same epoch)
3. Force evaluate mark snapshot and ssStakeMarkPoolDistr (bang pattern, not a data mutation)
4. `trans RUPD` — updates `nesRu` (reward update pulser state only)

## Intra-Epoch Case (no epoch crossing)

Fields that change in `NewEpochState`:
- `nesRu` — reward update pulser advances one step (or stays SNothing before stability window)
- `cgsFuturePParams` inside GovState — may convert Potential → Definite if past point-of-no-return
- `nesEL`, `nesBprev`, `nesBcur`, `nesPd`, `nesEs` — ALL UNCHANGED

For the forge path specifically:
- `nesPd` (pool distribution) — UNCHANGED, same value as parent block
- `curPParams` (via `getPParams = view $ newEpochStateGovStateL . curPParamsGovStateL`) — UNCHANGED
- Therefore: `tickedPP == untickedPP` and `tickedPd == untickedPd` for every intra-epoch slot

## Epoch Boundary Case

`newEpochTransition` runs when `eNo == succ eL`. In Conway:

### Step 1: applyRUpd (es0 → es1)
Modifies in EpochState:
- `casReserves` and `casTreasury` in ChainAccountState
- `utxosFeesL` (adds deltaF, which is negative: subtracts the fees)
- `certDStateL . accountsL` (adds staking rewards to registered accounts)
- `nonMyopic`

Does NOT touch `esSnapshots`. Confirmed by `applyRUpdFiltered`:
```haskell
EpochState as' ls' ss nm'  -- ss = esSnapshots unchanged
```

### Step 2: EPOCH (es1 → es2), which contains:

#### 2a. SNAP
Rotates snapshots:
```haskell
ssStakeMark = istakeSnap          -- new mark from current instantStake
ssStakeMarkPoolDistr = calculatePoolDistr istakeSnap  -- memoized pool distr
ssStakeSet = old ssStakeMark      -- old mark becomes set
ssStakeGo  = old ssStakeSet       -- old set becomes go
ssFee = utxosFees (post-applyRUpd fees)
```

#### 2b. POOLREAP
Returns pool deposits, removes retired pools from certPState. Modifies certState, utxoState, chainAccountState.

#### 2c. Conway RATIFY results applied
- Proposals pruned (enacted/expired removed)
- `cgsCommittee` updated
- `cgsConstitution` updated
- `cgsCurPParams` ← `nextEpochPParams govState0` (the new PParams if enacted, else unchanged)
- `cgsPrevPParams` ← old curPParams
- `cgsFuturePParams` ← `PotentialPParamsUpdate Nothing` (reset)
- Treasury withdrawals from enacted actions applied to ChainAccountState + DState

#### 2d. HARDFORK (conditional)
Only runs if ProtVer changed. Updates `nesEs` again.

#### 2e. setFreshDRepPulsingState
Uses `stakePoolDistr = ssStakeMarkPoolDistr snapshots1` (the NEW mark pool distr post-SNAP) to seed the DRep pulser for the coming epoch. This is stored inside `cgsDRepPulsingState` of the GovState, NOT in `nesPd`.

### Step 3: Record update (final NewEpochState construction)

```haskell
nesPd = ssStakeMarkPoolDistr (esSnapshots es0)
```

**CRITICAL**: `es0` is the EpochState BEFORE `applyRUpd` and BEFORE EPOCH runs. So `nesPd` is the OLD mark snapshot pool distribution (which was computed at the PREVIOUS epoch boundary from what was then the instantStake).

After the boundary:
- `nesPd` = pool distribution derived from the mark snapshot taken at the PREVIOUS epoch boundary (i.e. the same thing that was `ssStakeSet` before SNAP ran, or equivalently what became the new `ssStakeMarkPoolDistr` when SNAP ran — but from the PREVIOUS epoch's SNAP, not the current one)

Wait — more precisely: at epoch N boundary, `es0.esSnapshots.ssStakeMarkPoolDistr` was set at epoch N-1's SNAP. So `nesPd` after the epoch N boundary = pool distribution from epoch N-1's SNAP = the stake distribution as of epoch N-1 boundary.

This is the SAME `ssStakeMarkPoolDistr` that TICKF reads: `pd' = ssStakeMarkPoolDistr ss` where `ss = esSnapshots es` (pre-NEWEPOCH). These agree because TICKF skips SNAP.

### Fields the forge path reads from the TICKED state

From `protocolLedgerView` (Praos instance in SupportsProtocol.hs):
```haskell
Praos.LedgerView
  { lvPoolDistr    = nesPd                           -- from NewEpochState
  , lvMaxBodySize  = pparam LedgerCore.ppMaxBBSizeL  -- from getPParams = curPParamsGovStateL
  , lvMaxHeaderSize = pparam LedgerCore.ppMaxBHSizeL
  , lvProtocolVersion = pparam LedgerCore.ppProtocolVersionL
  }
```

From `txsMaxBytes` / `blockCapacityAlonzoMeasure` (mempool capacity):
```haskell
getPParams tickedShelleyLedgerState ^. ppMaxBBSizeL
getPParams tickedShelleyLedgerState ^. ppMaxBlockExUnitsL
getPParams tickedShelleyLedgerState ^. ppMaxTxSizeL
getPParams tickedShelleyLedgerState ^. ppMaxTxExUnitsL
getPParams tickedShelleyLedgerState ^. ppMaxRefScriptSizePerBlockG
```

Where `getPParams nes = nes ^. newEpochStateGovStateL . curPParamsGovStateL`.

## Minimum-Correct Forecast for Forge

When `epochOf(currentSlot) == epochOf(tip)` (intra-epoch):
- `nesPd` = parent block's `nesPd` (no change)
- `curPParams` = parent block's `curPParams` (no change, unless past point-of-no-return and a param update was ratified — in which case `cgsFuturePParams` becomes Definite, but `curPParamsGovStateL` still does NOT change until the actual epoch boundary)
- No state mutations needed. Just copy the unticked values.

When `epochOf(currentSlot) > epochOf(tip)` (epoch boundary):
- `nesPd` must become `ssStakeMarkPoolDistr(esSnapshots.before_SNAP)` — the OLD mark pool distr
  - Equivalently: the `ssStakeSet` pool distr that was computed at the PREVIOUS epoch's SNAP
  - In Dugite: this is the pool distr from `snapshots.mark` BEFORE the snapshot rotation
- `curPParams` must become `nextEpochPParams govState`:
  - If `cgsFuturePParams == DefinitePParamsUpdate pp` → new curPParams = pp
  - Otherwise (NoPParamsUpdate or Potential) → curPParams unchanged
  - Note: `DefinitePParamsUpdate` only appears after `solidifyFuturePParams` runs (2 stability windows before epoch end)
  - For the forge path, the relevant question is whether a ParameterChange or HardForkInitiation was ratified in the PREVIOUS epoch. If yes and it's past point-of-no-return, pparams change.

## Conway-Specific TICKF (Forecast) Path

`ConwayTICKF` explicitly skips SNAP, POOLREAP, RATIFY, and HARDFORK:
```haskell
-- We can skip 'SNAP'; we already have the equivalent pd'.
-- We can skip 'POOLREAP'; ...
pure $! nes {nesPd = pd'}
  & newEpochStateGovStateL . curPParamsGovStateL .~ nextEpochPParams govState
  & newEpochStateGovStateL . prevPParamsGovStateL .~ curPParams
  & newEpochStateGovStateL . futurePParamsGovStateL .~ NoPParamsUpdate
```

This is the canonical minimal-mutation list for consensus use. Only 3 things change at epoch boundary that the forecast cares about:
1. `nesPd` ← `ssStakeMarkPoolDistr ss` (pre-SNAP mark pool distr)
2. `curPParams` ← `nextEpochPParams govState` (i.e. enacted PP if any, else unchanged)
3. `prevPParams` ← old curPParams (consensus doesn't read this)

## Treasury/Reserves/Rewards at Epoch Boundary

These affect `casReserves`, `casTreasury`, `certDStateL.accountsL` (reward balances). The forge path reads NONE of these — they affect future snapshot stake (which influences next-next-epoch rewards) but not the current `nesPd` or `curPParams`. Your hypothesis is correct: treasury/reserves/rewards do not affect any value the forge path reads.

## UTxO at Epoch Boundary

`applyRUpd` does NOT touch the UTxO set. SNAP does not touch UTxO. POOLREAP moves deposits to outputs (modifies UTxO) but this doesn't affect forging — the forge path does not read the UTxO from the ticked state for the leader check or pparams. Your hypothesis is correct.

## Gov Action Enactment at Epoch Boundary

Conway EPOCH applies RATIFY results:
- ParameterChange enacted → `cgsCurPParams` updated (forge path reads this via getPParams)
- HardForkInitiation enacted → ProtVer in PParams changes, and HARDFORK rule runs
- NoConfidence, UpdateCommittee, NewConstitution, TreasuryWithdrawals, InfoAction → affect committee/constitution/treasury, NOT curPParams or nesPd directly

So for the forge path: only ParameterChange and HardForkInitiation enactments are relevant. All others are invisible to `nesPd`/`curPParams`.

## tickedPP == untickedPP Question

Definitive answer:
- **Intra-epoch (no epoch crossing)**: `tickedPP == untickedPP` always. `curPParamsGovStateL` is never mutated by TICK alone (only `cgsFuturePParams` potentially solidifies, but that doesn't change `curPParamsGovStateL`).
- **Epoch boundary**: `tickedPP == untickedPP` UNLESS a ParameterChange or HardForkInitiation gov action was ratified in the previous epoch AND it was past the point-of-no-return (2 stability windows before epoch end). In the common case (no ratified PP update), they are equal even at epoch boundaries.
- The accessor `getPParams` always reads `newEpochStateGovStateL . curPParamsGovStateL`, which is the same field in both ticked and unticked state for intra-epoch ticks.
