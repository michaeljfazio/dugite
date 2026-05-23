# Shelley Core Ledger Rules — LEDGER, EPOCH, NEWEPOCH, TICK

Source: `IntersectMBO/cardano-ledger` master (commit `ebed62de1ebcd4b13512418d49d17802a193e2c1`).

## 0. State types

`eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/Types.hs`

```haskell
data NewEpochState era = NewEpochState
  { nesEL              :: !EpochNo
  , nesBprev           :: !BlocksMade            -- blocks made in PREV epoch
  , nesBcur            :: !BlocksMade            -- blocks made in CUR epoch
  , nesEs              :: !(EpochState era)
  , nesRu              :: !(StrictMaybe PulsingRewUpdate)
  , nesPd              :: !PoolDistr             -- for leader VRF
  , stashedAVVMAddresses :: !(StashedAVVMAddresses era)
  }

data EpochState era = EpochState
  { esChainAccountState :: !ChainAccountState   -- reserves + treasury
  , esLState            :: !(LedgerState era)
  , esSnapshots         :: !SnapShots
  , esNonMyopic         :: !NonMyopic
  }

data LedgerState era = LedgerState
  { lsUTxOState :: !(UTxOState era)
  , lsCertState :: !(CertState era)
  }

data UTxOState era = UTxOState
  { utxosUtxo, utxosDeposited, utxosFees, utxosGovState
  , utxosInstantStake, utxosDonation }

data ChainAccountState = ChainAccountState { casReserves, casTreasury }

data SnapShots = SnapShots
  { ssStakeMark, ssStakeMarkPoolDistr, ssStakeSet, ssStakeGo, ssFee }
```

## 1. LEDGER (per-tx)

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ledger.hs`

**Env**: `LedgerEnv { ledgerSlotNo, ledgerEpochNo, ledgerIx, ledgerPp, ledgerAccount }`. `ledgerEpochNo` is `Just e` from LEDGERS (pre-computed), `Nothing` → derived from slot.

**State**: `LedgerState era` (UTxO + Cert).

**Signal**: `StAnnTx TopTx era` (one annotated tx).

**Pre-conditions (before sub-rules)**:
1. Every withdrawal address must have a registered account: `ShelleyWithdrawalsMissingAccounts`.
2. Each withdrawal must drain its account to zero: `ShelleyIncompleteWithdrawals`.

**Sub-rules in order**:
1. `testIncompleteAndMissingWithdrawals` (pre-check)
2. `DELEGS` — receives state `(certState & certDStateL.accountsL %~ drainAccounts withdrawals)` (drained first). Processes `tx.body.certs`.
3. `UTXOW` — receives the **original** `certState` (not post-DELEGS). So script witness checks see the pre-DELEGS pool set.

**Post assertions**: `utxosDeposited == totalObligation certState govState`; reverse-delegation mapping consistent.

**Predicate failures**: `UtxowFailure`(0), `DelegsFailure`(1), `ShelleyWithdrawalsMissingAccounts`(2), `ShelleyIncompleteWithdrawals`(3).

## 2. UTXOW

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxow.hs`

Checks in order:
1. `validateFailedNativeScripts` — `ScriptWitnessNotValidatingUTXOW`
2. `validateMissingScripts` — `MissingScriptWitnessesUTXOW` / `ExtraneousScriptWitnessesUTXOW`
3. `validateVerifiedWits` — `InvalidWitnessesUTXOW`
4. `validateNeededWitnesses` — `MissingVKeyWitnessesUTXOW` (incl. inputs, withdrawals, certs, pool ops, genesis delegates)
5. `validateMetadata` — `MissingTxBodyMetadataHash` / `MissingTxMetadata` / `ConflictingMetadataHash` / `InvalidMetadata`
6. `validateMIRInsufficientGenesisSigs` — `MIRInsufficientGenesisSigsUTXOW` (if MIR certs present, ≥ quorum)
7. `trans @UTXO` — call UTXO sub-rule

Failure tags: 0-3 as above, 4=`UtxoFailure`, 5=`MIRInsufficientGenesisSigsUTXOW`, 6-10 metadata + extraneous.

## 3. UTXO

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Utxo.hs`

**Env**: `UtxoEnv { ueSlot, uePParams, ueCertState }`.

**Checks in order**:
1. TTL: `tx.body.ttl >= slot` — `ExpiredUTxO`
2. Inputs non-empty — `InputSetEmptyUTxO`
3. `minfee pp tx <= tx.body.fee` — `FeeTooSmallUTxO`
4. `inputs ⊆ dom(utxo)` — `BadInputsUTxO`
5. Outputs' networkId matches — `WrongNetwork`
6. Withdrawals' networkId matches — `WrongNetworkWithdrawal`
7. Value conservation: `consumed == produced` (UTxO value + key deposit refunds + withdrawals = outputs + fee + key deposits + pool deposits) — `ValueNotConservedUTxO`
8. **PPUP sub-rule** — replaces `utxosGovState`
9. Min UTxO value: every output coin ≥ `minUTxOValue` — `OutputTooSmallUTxO`
10. Bootstrap address attrs ≤ 64 bytes — `OutputBootAddrAttrsTooBig`
11. Tx size ≤ `maxTxSize` — `MaxTxSizeUTxO`

**State update** (`updateUTxOState`):
```
newUTxO = (utxo - inputs) ∪ outputs
depositChange = certsTotalDepositsTxBody - certsTotalRefundsTxBody
utxosDeposited += depositChange
utxosFees      += tx.body.fee
utxosGovState  ← ppup'
utxosInstantStake updated (delete consumed + add new outputs)
```

**Failure tags**: 0=`BadInputsUTxO`, 1=`ExpiredUTxO`, 2=`MaxTxSizeUTxO`, 3=`InputSetEmptyUTxO`, 4=`FeeTooSmallUTxO`, 5=`ValueNotConservedUTxO`, 6=`OutputTooSmallUTxO`, 7=`UpdateFailure` (PPUP), 8=`WrongNetwork`, 9=`WrongNetworkWithdrawal`, 10=`OutputBootAddrAttrsTooBig`.

## 4. LEDGERS (block-level)

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Ledgers.hs`

**Env**: `LedgersEnv { ledgersSlotNo, ledgersEpochNo, ledgersPp, ledgersAccount }`.

**Signal**: `Seq (Tx TopTx era)` — block's transactions.

Left-fold via LEDGER. `stAnnTx` is recomputed from the **current** UTxO after each tx (so each tx sees previous txs' outputs). Tx index starts at `minBound :: TxIx` (= 0).

## 5. TICK

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Tick.hs`

Called once per block header before LEDGERS.

**`bheadTransition` order**:
1. `solidifyNextEpochPParams` — at `slot >= slotOfNoReturn` (= `firstSlotNextEpoch - 2*stabilityWindow`), transition `sgsFuturePParams` from `PotentialPParamsUpdate (Just pp)` → `DefinitePParamsUpdate pp`.
2. `trans @NEWEPOCH` — if `curEpochNo == succ(nesEL)`, fires full body; else no-op.
3. `adoptGenesisDelegs` — promote `FutureGenDeleg` entries with `slot <= currentSlot` (latest-wins per gkh).
4. Force WHNF of `ssStakeMark` and `ssStakeMarkPoolDistr` (TICK only, NOT TICKF).
5. `trans @RUPD (TRC (RupdEnv bprev es, nesRu nes1, slot))` — uses `bprev` from `nes0` (pre-NEWEPOCH).

**Critical**: `bprev` captured from `nes0` BEFORE NEWEPOCH (because NEWEPOCH rotates `nesBprev ← nesBcur`).

**RUPD timing**:
- `sr = randomnessStabilisationWindow = ceil(4k/f)`
- `slot      = epochInfoFirst e + sr`
- `slotForce = slot + sr` = `epochStart + 8k/f`
- `RewardsTooEarly` (s ≤ epoch_start + sr): `SNothing` (discards any in-progress pulser)
- `RewardsJustRight` (sr < s ≤ 2sr): start or continue pulsing
- `RewardsTooLate` (s > 2sr): force completion

`PredicateFailure (ShelleyTICK era) = Void`.

**TICKF**: skips RUPD, skips snapshot forcing. For epoch boundaries inlines only UPEC. For non-boundaries only `adoptGenesisDelegs`.

## 6. NEWEPOCH

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/NewEpoch.hs`

**Guard**: only fires when `eNo == succ(nesEL)`.

**Order (when fired)**:
1. `applyRUpd` / `completeRupd` — apply `nesRu` to `EpochState`. If still `Pulsing`, `liftSTS . completeRupd` first.
2. `trans @MIR ()` — apply pending MIR certificates accumulated via DELEG into `dsIRewards`.
3. `trans @EPOCH eNo` → calls SNAP → POOLREAP → UPEC.
4. `let pd' = ssStakeMarkPoolDistr (esSnapshots es)` — **uses pre-EPOCH** snapshot. After SNAP rotates, this equals `calculatePoolDistr(ssStakeSet of es''')` (memoized).
5. Update `nesEL ← eNo`, `nesBprev ← bcur`, `nesBcur ← mempty`, `nesEs ← es'''`, `nesRu ← SNothing`, `nesPd ← pd'`.

`PredicateFailure (ShelleyNEWEPOCH era) = Void`.

## 7. EPOCH

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Epoch.hs`

**Sub-rules in order**:
1. `trans @SNAP ()` → rotate snapshots, take new mark from `instantStake`.
2. `trans @POOLREAP eNo` → activate future pool params; retire pools with `psRetiring[pid] == e`; refund deposits (registered → account, unregistered → treasury); clear delegations.
3. **Recompute deposit pot**: `utxosDeposited = totalObligation adjustedCertState govState`.
4. `trans @UPEC ()` → NEWPP applies the voted-PP update.
5. Assemble: `prevPParams ← pp`, `curPParams ← pp'`.

`PredicateFailure (ShelleyEPOCH era) = Void`.

## 8. Full epoch-boundary chain

```
TICK (slot → NewEpochState):
  solidifyNextEpochPParams
  NEWEPOCH (eNo → NewEpochState):
    [guard: eNo == succ nesEL]
    applyRUpd / completeRupd       →  EpochState (rewards applied)
    MIR ()                         →  EpochState (MIR distributed)
    EPOCH (eNo → EpochState):
      SNAP ()                      →  SnapShots (mark/set/go rotated, ssFee captured)
      POOLREAP eNo                 →  PoolreapState (activate + retire)
      recompute utxosDeposited = totalObligation
      UPEC ()                      →  UpecState:
        NEWPP (pp → NewppState):
          apply voted PP update; rotate future→current proposals
      prevPParams ← pp; curPParams ← pp'
    pd' = ssStakeMarkPoolDistr(esSnapshots BEFORE EPOCH)
    nesEL ← eNo; nesBprev ← nesBcur; nesBcur ← ∅; nesRu ← SNothing; nesPd ← pd'
  adoptGenesisDelegs (force ssStakeMark, ssStakeMarkPoolDistr)
  RUPD slot → StrictMaybe PulsingRewUpdate
```

## Key invariants

1. **RUPD uses `nesBprev` from `nes0`** (pre-NEWEPOCH). After NEWEPOCH, `nesBprev` has been rotated.
2. **`applyRUpd` before SNAP**: rewards applied to accounts BEFORE mark snapshot takes a picture. Mark from `instantStake` (UTxO-derived).
3. **`applyRUpd` before MIR**: MIR additive on top.
4. **POOLREAP before UPEC**: new PP only effective from next block.
5. **`prevPParams ← pp` in EPOCH**: stored just before `curPParams ← pp'`. Gives next epoch's RUPD the correct expansion params.
6. **`ssFee` in SNAP**: fees from current ending epoch. 2-epoch delay until enters reward calc as `ssStakeGo.ssFee`.

## Rust translation notes for dugite

- Withdrawal drain happens BEFORE DELEGS; drain into temp structure.
- UTXOW must receive **pre-DELEGS** certState (for script witness checks). Use a borrow of the pre-DELEGS state.
- `StAnnTx` rebuilt per-tx from the **current** UTxO (not block's original).
- `bprev` captured at top of `bheadTransition` BEFORE NEWEPOCH.
- TICKF skips RUPD entirely.
- NEWEPOCH guard `eNo /= succ eNoL` → preserve entire state unchanged (don't reset `nesRu`).
- `completeRupd` is synchronous; finish any pending pulser inline at boundary.
- `pd'` from **pre-EPOCH** snapshots; after SNAP `ssStakeMarkPoolDistr` is the new mark distr (2 epochs from now).
- SNAP uses `instantStake` (incremental), not full UTxO scan.
- POOLREAP activates `psFutureStakePoolParams` BEFORE retirement check; activation preserves existing deposit and delegators.
- `utxosDeposited` recompute is full from `totalObligation`, not incremental.
- `prevPParams ← curPParams` at very end of EPOCH — required for RUPD next epoch.
