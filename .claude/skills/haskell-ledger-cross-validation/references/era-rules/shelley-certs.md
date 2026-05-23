# Shelley Certs + PPUP + TICK Nonce Evolution — Reference

Source: `IntersectMBO/cardano-ledger` master + `IntersectMBO/ouroboros-consensus` master.

## Cert dispatch hierarchy

```
LEDGER → DELEGS (one call per cert, left-to-right)
           └── DELPL (fan-out by cert type)
                 ├── DELEG (StakeReg, StakeDeleg, StakeDereg, MIR, GenesisDeleg)
                 └── POOL  (PoolReg, PoolRet)
```

`DELPL` (`eras/shelley/impl/.../Rules/Delpl.hs`) pattern-matches on `TxCert era`:
- `ShelleyTxCertPool` → POOL with `certPStateL`
- `ShelleyTxCertGenesisDeleg` → DELEG with full `CertState`
- `ShelleyTxCertDelegCert` → DELEG with full `CertState`

**Every cert in a tx is processed immediately at tx-apply time** against the current ledger state. Effects land in `CertState`. Deferred effects (pool retirement, future genesis delegations) manifest as deferred scheduling entries, not suspended rules.

---

## 1. DELEG

`eras/shelley/impl/.../Rules/Deleg.hs`

**Env**: `DelegEnv { slotNo, deCurEpochNo, ptr_ :: Ptr, deChainAccountState, ppDE }`

`ptr_ = (slotNo, txIx, certIx)` — used as pointer address for new stake registrations.

### 1.1 `RegTxCert cred` — StakeRegistration

- **Effect timing**: immediate.
- **Predicate**: `not (isAccountRegistered cred)` else `StakeKeyAlreadyRegisteredDELEG`.
- **Mutation**: `accountsL %~ registerShelleyAccount cred ptr compactDeposit Nothing`.
- Inserts into `accountsL`, records compact deposit + pointer address. Reward balance = 0.

### 1.2 `UnRegTxCert cred` — StakeDeregistration

- **Effect timing**: immediate.
- **Predicates** (order):
  1. If found but balance non-zero: `StakeKeyNonZeroAccountBalanceDELEG balance`
  2. If not found: `StakeKeyNotRegisteredDELEG cred`
- **Mutation**: `unregisterShelleyAccount` removes from `accountsL`; `unDelegReDelegStakePool` clears stake delegation from `PState`.
- **Deposit refund**: NOT here. UTxO rule handles via deposit pot accounting.

### 1.3 `DelegStakeTxCert cred stakePool`

- **Effect timing**: immediate.
- **Predicates**:
  1. `Map.member stakePool psStakePools` else `DelegateeNotRegisteredDELEG`. (NB: checks `psStakePools`, NOT `psFutureStakePoolParams` — a pool registered earlier in the same tx is in `psStakePools` already.)
  2. `lookupAccountStateIntern cred accountsL` returns Just, else `StakeDelegationImpossibleDELEG`.
- **Mutation**:
  ```
  accountsL %~ adjustAccountState (stakePoolDelegationAccountStateL ?~ stakePool) cred
  certPStateL %~ unDelegReDelegStakePool internedCred accountState (Just stakePool)
  ```
  Writes new pool target into account; updates `PState`'s reverse delegator-set mapping.

### 1.4 `GenesisDelegTxCert gkh vkh vrf`

- **Effect timing**: deferred by `stabilityWindow` slots, then activated by `adoptGenesisDelegs` in TICK.
- **Deferral**:
  ```
  s' = slot +* Duration stabilityWindow
  dsFutureGenDelegsL .= Map.insert (FutureGenDeleg s' gkh) (GenDelegPair vkh vrf) old
  ```
- **Predicates**:
  1. `GenesisKeyNotInMappingDELEG gkh` if `gkh ∉ dsGenDelegs`
  2. `DuplicateGenesisDelegateDELEG vkh` if new cold key clashes
  3. `DuplicateGenesisVRFDELEG vrf` if new VRF key clashes
- **Activation** (in TICK `adoptGenesisDelegs`): partition entries with `slot <= currentSlot`. For each gkh appearing multiple times, **latest-wins** (by slot).

---

## 2. POOL

`eras/shelley/impl/.../Rules/Pool.hs`

**State**: `PState era` (`psStakePools`, `psFutureStakePoolParams`, `psRetiring`, `psVRFKeyHashes`)

**Env**: `PoolEnv eNo pp`.

### 2.1 `RegPool stakePoolParams`

Predicates (some hardfork-gated):
1. `hardforkAlonzoValidatePoolAccountAddressNetID pv`: reward account networkId → `WrongNetworkPOOL`
2. `SoftForks.restrictPoolMetadataHash pv`: metadata hash ≤ 32 bytes → `PoolMedataHashTooBig`
3. `sppCost >= pp.ppMinPoolCost` → `StakePoolCostTooLowPOOL`
4. `hardforkConwayDisallowDuplicatedVRFKeys pv`: VRF key not in use → `VRFKeyHashAlreadyRegistered`

**Two cases**:
- **New pool** (`pid ∉ psStakePools`):
  ```
  psStakePoolsL %~ Map.insert pid (mkStakePoolState ppPoolDeposit mempty params)
  psVRFKeyHashesL increment refcount
  ```
  Deposit taken from PParams. Pool visible in snapshots from NEXT epoch boundary.
- **Re-registration** (already in `psStakePools`):
  ```
  psFutureStakePoolParamsL %~ Map.insert pid params
  psRetiringL %~ Map.delete pid          -- cancels pending retirement
  psVRFKeyHashesL updateFuture
  ```
  Promoted to `psStakePools` at next epoch boundary in POOLREAP. **Deposit unchanged** on re-reg.

### 2.2 `RetirePool pid e`

Predicates:
1. `pid ∈ psStakePools` else `StakePoolNotRegisteredOnKeyPOOL`
2. `cEpoch < e && e <= cEpoch + ppEMax` else `StakePoolRetirementWrongEpochPOOL`

**Mutation**: `psRetiringL %~ Map.insert pid e`. Only scheduling. Actual removal/refund in POOLREAP at boundary for epoch `e`.

---

## 3. PPUP

`eras/shelley/impl/.../Rules/Ppup.hs`

`AtMostEra "Babbage" era` — exists only Shelley through Babbage. Conway replaces with governance.

**State**: `ShelleyGovState era`:
- `sgsCurProposals :: ProposedPPUpdates era`
- `sgsFutureProposals :: ProposedPPUpdates era`
- `sgsFuturePParams :: FuturePParams era`

**Env**: `PPUPEnv slot pp (GenDelegs genDelegs)`

**Signal**: `StrictMaybe (Update era)` — `SNothing` no-op; `SJust (Update (ProposedPPUpdates pup) targetEpochNo)` carries vote.

**Predicates**:
1. `pup` keys ⊆ `genDelegs` keys → `NonGenesisUpdatePPUP` (only genesis delegates vote)
2. Every `PParamsUpdate` in `pup` must pass `hasLegalProtVerUpdate pp` (only `(major+1, 0)` or `(major, minor+1)`) → `PVCannotFollowPPUP`

**The `tooLate` slot**:
```
tooLate = firstSlotNextEpoch *- Duration (2 * stabilityWindow)
        = firstSlotNextEpoch - 6k/f slots
```

(Distinct from MIR which uses only 1 stability window.)

**Two voting windows**:

**Before `tooLate`** (vote for CURRENT epoch):
```
targetEpochNo == curEpochNo else PPUpdateWrongEpoch ... VoteForThisEpoch
curProposals = Map.union pup pupS    -- left-biased: new vote overrides
sgsFuturePParams = PotentialPParamsUpdate $ votedFuturePParams curProposals pp coreNodeQuorum
```

**At/after `tooLate`** (vote for NEXT epoch):
```
succ curEpochNo == targetEpochNo else PPUpdateWrongEpoch ... VoteForNextEpoch
sgsFutureProposals = Map.union pup fpupS
```

**Quorum check** (`votedFuturePParams`):
```haskell
votedFuturePParams (ProposedPPUpdates pppu) pp quorumN = do
  let votes = Map.foldr (\v -> Map.insertWith (+) v 1) Map.empty pppu
      consensus = Map.filter (>= quorumN) votes
  [ppu] <- Just $ Map.keys consensus
  let ppNew = applyPPUpdates pp ppu
  guard $ ppNew.maxTxSize + ppNew.maxBHSize < ppNew.maxBBSize
  pure ppNew
```

`quorumN = Globals.quorum` (strictly > half genesis nodes). Size invariant check at the end.

`sgsFuturePParams = PotentialPParamsUpdate (Just ppNew)` if quorum reached, else `PotentialPParamsUpdate Nothing`.

---

## 4. NEWPP (called from UPEC)

`eras/shelley/impl/.../Rules/Newpp.hs`

**State**: `ShelleyNewppState era = NewppState (PParams era) (ShelleyGovState era)`.

**Signal**: `PParams era` = `nextEpochPParams ppupState` (winning update applied to curPParams, or curPParams unchanged).

```haskell
updatePpup coreNodeQuorum ppupState pp =
  NewppState pp $ ppupState
    { sgsCurProposals    = curProposals     -- future → current (if all legal)
    , sgsFutureProposals = emptyPPPUpdates
    , sgsFuturePParams   = PotentialPParamsUpdate $
        votedFuturePParams curProposals pp coreNodeQuorum
    }
  where
    ProposedPPUpdates newProposals = sgsFutureProposals ppupState
    curProposals =
      if all (hasLegalProtVerUpdate pp) newProposals
        then ProposedPPUpdates newProposals
        else emptyPPPUpdates        -- discard entire set if any illegal
```

1. Promote `sgsFutureProposals` → `sgsCurProposals` (drop entire set if any proposal can't follow new `pp`).
2. Clear `sgsFutureProposals`.
3. Recompute `sgsFuturePParams` on new `curProposals` × new `pp` × quorum.

**Pipeline timing**:
```
TICK → solidifyNextEpochPParams → NEWEPOCH → MIR → EPOCH → SNAP → POOLREAP → UPEC → NEWPP
```

`solidifyNextEpochPParams` (in `Tick.hs`) runs each block once `slot >= slotOfNoReturn`. Transitions `sgsFuturePParams` from `PotentialPParamsUpdate` → `FuturePParamsUpdate` ("point of no return").

---

## 5. POOLREAP

`eras/shelley/impl/.../Rules/PoolReap.hs`

Fires at each epoch boundary (in EPOCH, after SNAP, before UPEC). Signal: epoch `e`.

### Step 1 — activate future pool params
```
ps.psStakePools = Map.merge ... psFutureStakePoolParams psStakePools
ps.psFutureStakePoolParams = Map.empty
```
Existing `StakePoolState`'s deposit + delegator set preserved; only `StakePoolParams` updated.

### Step 2 — identify retiring pools
```
retired = { pid | psRetiring[pid] == e }
retiringPools = restrict psStakePools to retired
```

### Step 3 — collect deposit refunds (by reward account)
```
accountRefunds = Map.fromListWith (<>)
  [(unAccountId spsAccountId, spsDeposit) | sps in retiringPools]
```

### Step 4 — partition into claimable / unclaimed
```
(refunds, unclaimedDeposits) = partition by isAccountRegistered
```

### Step 5 — apply mutations
```
utxosDeposited -= refunded + unclaimed
casTreasury    += unclaimed                    -- ← unclaimed go to TREASURY (not reserves)
accountsL      %~ removeStakePoolDelegations (delegsToClear cs retired)
                . addToBalanceAccounts refunds
psStakePoolsL  %~ withoutKeys retired
psRetiringL    %~ withoutKeys retired
psVRFKeyHashesL adjust counts
```

**Post-conditions**:
- `potEqualsObligation`
- Account count unchanged

---

## 6. MIR

`eras/shelley/impl/.../Rules/Mir.hs`

Runs in NEWEPOCH between `applyRUpd` and EPOCH:
```
NEWEPOCH: applyRUpd → MIR → EPOCH (SNAP → POOLREAP → UPEC)
```

**Signal**: `()` (rule reads from `dsIRewards`).

`InstantaneousRewards` accumulates throughout the epoch (via DELEG):
```haskell
data InstantaneousRewards = InstantaneousRewards
  { iRReserves     :: Map (Cred Staking) Coin   -- reserves→account pending
  , iRTreasury     :: Map (Cred Staking) Coin   -- treasury→account pending
  , deltaReserves  :: DeltaCoin                  -- pot-to-pot delta
  , deltaTreasury  :: DeltaCoin                  -- (invariant: dR + dT = 0)
  }
```

`StakeAddressesMIR` certs merge via `Map.unionWith (<>)` (Alonzo+). `SendToOppositePotMIR` certs adjust `deltaReserves`/`deltaTreasury`.

**Rule logic**:
```haskell
irwdR = iRReserves `Map.intersection` accountsMap   -- filter to registered
irwdT = iRTreasury `Map.intersection` accountsMap
totR  = fold irwdR
totT  = fold irwdT
availableReserves = reserves + deltaReserves
availableTreasury = treasury + deltaTreasury
update = irwdR `Map.unionWith (<>)` irwdT
```

**Solvency check** (all-or-nothing across both pots):
- `totR <= availableReserves && totT <= availableTreasury`

**If solvent**:
```
casReserves = availableReserves - totR
casTreasury = availableTreasury - totT
accounts += update
dsIRewards = emptyInstantaneousRewards
```

**If insolvent**: emit `NoMirTransfer`; still clear `dsIRewards`; pots unchanged.

**Payments to deregistered accounts**: silently dropped (Map.intersection filter). Coin vanishes (NOT returned to pot — distinct from POOLREAP's "unclaimed → treasury").

**`checkSlotNotTooLate` (from DELEG)**: MIR certs accepted only if `slot < tooLate` where `tooLate = epochInfoFirst newEpoch *- Duration stabilityWindow` (1 stability window — distinct from PPUP's 2 stability windows).

**`SendToOppositePotMIR`**: Pre-Alonzo blocked with `MIRTransferNotCurrentlyAllowed`. Post-Alonzo adjusts `iRDeltaReserves`/`iRDeltaTreasury` opposites (sum zero).

---

## 7. PPUP/UPEC full lifecycle

1. **Genesis delegates vote during epoch E**. Before `tooLate` → `sgsCurProposals` + recompute `sgsFuturePParams`. After → `sgsFutureProposals`.
2. **`solidifyNextEpochPParams`** at each block once `slot >= slotOfNoReturn`: `sgsFuturePParams` Potential → Definite ("point of no return").
3. **E→E+1 boundary, NEWEPOCH**: RUPD completion → MIR → EPOCH → SNAP → POOLREAP → UPEC → NEWPP.
4. **UPEC reads `nextEpochPParams`**: returns `ppNew` if `FuturePParamsUpdate (Just ppNew)`, else current pp.
5. **NEWPP**: gets `ppNew` as signal. Promotes `sgsFutureProposals` → `sgsCurProposals` (legality check); clears futures; recomputes `sgsFuturePParams`.
6. **EPOCH installs `pp'`** as `curPParamsEpochStateL`; stores old `pp` as `prevPParamsEpochStateL`.

---

## 8. TICK nonce evolution

`Tick.hs` + `ouroboros-consensus-protocol/.../Praos.hs` + `StabilityWindow.hs`

### Praos state
```haskell
data PraosState = PraosState
  { praosStateEvolvingNonce      :: Nonce   -- accumulates VRF every block
  , praosStateCandidateNonce     :: Nonce   -- snapshot, frozen at stability window
  , praosStateEpochNonce         :: Nonce   -- active for VRF leader check (eta0)
  , praosStatePreviousEpochNonce :: Nonce   -- previous, for Peras
  , praosStateLabNonce           :: Nonce   -- prev block hash nonce
  , praosStateLastEpochBlockNonce :: Nonce  -- labNonce of last block of prev epoch
  , ...
  }
```

### Per-block (`reupdateChainDepState`):
```haskell
eta = vrfNonceValue hbVrfRes
newEvolvingNonce = praosStateEvolvingNonce ⭒ eta

cs { praosStateLastSlot      = NotOrigin slot
   , praosStateLabNonce      = prevHashToNonce hvPrevHash
   , praosStateEvolvingNonce = newEvolvingNonce
   , praosStateCandidateNonce =
       if slot + praosRandomnessStabilisationWindow < firstSlotNextEpoch
         then newEvolvingNonce         -- still within freeze window
         else praosStateCandidateNonce -- past freeze: candidate locked
   , praosStateOCertCounters = Map.insert hk n ...
   }
```

**Candidate freeze**: stops updating when current slot within `4k/f` of next epoch boundary. Last block contributing to next epoch's nonce is approx `firstSlotNextEpoch - 4k/f - 1`.

### Epoch transition (`tickChainDepState` when `isNewEpoch`):
```haskell
st { praosStateEpochNonce =
       praosStateCandidateNonce ⭒ praosStateLastEpochBlockNonce
   , praosStatePreviousEpochNonce = praosStateEpochNonce
   , praosStateLastEpochBlockNonce = praosStateLabNonce
   }
```

`epochNonce = candidateNonce ⭒ lastEpochBlockNonce`. `lastEpochBlockNonce` = labNonce of the last block before boundary. This injects unpredictability — adversary can't know `lastEpochBlockNonce` ahead.

### Leader check
`checkIsLeader` uses `praosStateEpochNonce` (= eta0). Constructs `rho' = mkInputVRF slot eta0`. Evaluates VRF, compares to leader threshold.

### Stability windows
| Constant | Formula | Used for |
|---|---|---|
| `stabilityWindow` | `ceil(3k/f)` | Genesis delegation deferral, MIR deadline, PPUP tooLate |
| `randomnessStabilisationWindow` | `ceil(4k/f)` | VRF candidate freeze, RUPD start window |

Mainnet (k=2160, f=0.05): stabilityWindow ≈ 129,600 slots (~1.5 day); randomnessStabilisationWindow ≈ 172,800 slots (~2 days).

---

## Rust translation notes for dugite

### DELEG
- `DelegEnv.ptr_` = `(SlotNo, TxIx, CertIx)` — must thread cert index correctly within tx.
- `DelegateeNotRegisteredDELEG` checks `psStakePools` (NOT `psFutureStakePoolParams`). A pool registered earlier in same tx is in `psStakePools`.
- `StakeKeyNonZeroAccountBalanceDELEG` checked BEFORE `StakeKeyNotRegisteredDELEG`.

### POOL
- Re-registration → `psFutureStakePoolParams` not `psStakePools`. Active only after boundary.
- `psVRFKeyHashes` is reference count for duplicate-VRF prohibition (Conway+).
- `RetirePool` is pure scheduling — can fail only on missing-pool or invalid-epoch.

### PPUP / NEWPP
- `tooLate = 2 * stabilityWindow` from epoch end (= `6k/f` slots before).
- `sgsFuturePParams` has 3 states: `NoPParamsUpdate`, `PotentialPParamsUpdate (Maybe PParams)`, `FuturePParamsUpdate (Maybe PParams)`. Only `FuturePParamsUpdate` consumed by UPEC.
- `votedFuturePParams` quorum check fires on every new vote — cache, don't recompute.

### POOLREAP
- Retiring pools' stakers get delegations cleared. They stay registered but get no rewards until re-delegated.
- `utxosDeposited -= refunded + unclaimed`. Both leave deposit pot regardless of refund target.

### MIR
- Payment to deregistered account silently dropped (filtered by `Map.intersection`). Coin vanishes (≠ POOLREAP).
- Solvency check is all-or-nothing across BOTH pots simultaneously.
- MIR deprecated in Conway (`AtMostEra "Babbage"` constraint).

### Nonce
- `candidateNonce` frozen once `slot + 4k/f >= firstSlotNextEpoch`. Per-block check, no per-epoch flag.
- `lastEpochBlockNonce` set from `labNonce` at epoch tick. Store separately from `evolvingNonce`.
- `epochNonce = candidateNonce ⭒ lastEpochBlockNonce` — both needed at tick; capture before overwrite.
