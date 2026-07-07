---
name: mir-pot-transfer-semantics
description: Haskell MIR (Move Instantaneous Rewards) applyMIR/mirTransition exact pot-delta formula, solvency check, insolvency behavior, registered-cred filter, era removal (live-verified 2026-07-06)
metadata:
  type: reference
---

Live-verified via cardano-haskell-oracle against IntersectMBO/cardano-ledger master, 2026-07-06. Answers a dugite-ledger port question for `apply_pending_mir` (panic-on-insolvency bug fix).

## Two-phase design: standalone MIR rule, separate from EPOCH/NEWEPOCH

- **Accumulation** (mid-block, per `MIRCert`): `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`, `delegationTransition` → `updateReservesAndTreasury`. Writes into `DState.dsIRewards` (pending only).
- **Application** (epoch boundary): standalone `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Mir.hs`, `mirTransition`. Called from `NewEpoch.newEpochTransition` (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/NewEpoch.hs`) in order: `updateRewards (RUPD) → trans @(EraRule "MIR") → trans @(EraRule "EPOCH")`. MIR sees pots AFTER that epoch's monetary expansion has landed.

## InstantaneousRewards fields (`libs/cardano-ledger-core/src/Cardano/Ledger/State/CertState.hs`)

```haskell
data InstantaneousRewards = InstantaneousRewards
  { iRReserves :: !(Map (Credential Staking) Coin)
  , iRTreasury :: !(Map (Credential Staking) Coin)
  , deltaReserves :: !DeltaCoin
  , deltaTreasury :: !DeltaCoin
  }
-- invariant: deltaReserves + deltaTreasury = 0
```

## Pot delta formula — NOT cross-swapped (own-pot-name only)

```haskell
availableReserves = reserves `addDeltaCoin` deltaReserves (dsIRewards ds)
availableTreasury = treasury `addDeltaCoin` deltaTreasury (dsIRewards ds)
```
Cross-pot "move FROM one TO the other" semantics are baked into the *sign* of the delta fields at DELEG-time accumulation (`SendToOppositePotMIR` sets `iRDeltaReservesL <>~ invert(toDeltaCoin coin)` and `iRDeltaTreasuryL <>~ toDeltaCoin coin`), NOT into which field the epoch-boundary code reads. A naive `reserves + deltaTreasury - deltaReserves` cross-term formula is WRONG.

## Solvency check — single combined AND, not independent per-pot

```haskell
accountsMap = ds ^. accountsL . accountsMapL
irwdR = iRReserves (dsIRewards ds) `Map.intersection` accountsMap
irwdT = iRTreasury (dsIRewards ds) `Map.intersection` accountsMap
totR = fold irwdR
totT = fold irwdT
if totR <= availableReserves && totT <= availableTreasury
  then ...apply...
  else ...no-op, emit NoMirTransfer...
```
One boolean gate over BOTH pots — a treasury shortfall blocks an otherwise-solvent reserves payout in the same epoch, and vice versa. `type PredicateFailure (MIR era) = Void` and `type PredicateFailure (NEWEPOCH era) = Void` at this layer — this branch CANNOT throw an STS failure; it's a pure if/then/else producing only an informational event (`MirTransfer` vs `NoMirTransfer InstantaneousRewards Coin Coin`).

Contrast: a real throwable `InsufficientForInstantaneousRewardsDELEG pot (Mismatch supplied expected)` exists at the EARLIER, separate DELEG cert-processing-time check (`updateReservesAndTreasury`), using the UNFILTERED `combinedMap` against `available = potAmount [+ delta if hardforkAlonzoAllowMIRTransfer]`. That one really can reject a block (bad cert). The epoch-boundary MIR application is a different, total/non-failing check.

## Insolvency behavior — dropped forever, strictly atomic, no partial

```haskell
else do
  tellEvent $ NoMirTransfer (...) availableReserves availableTreasury
  pure $ EpochState chainAccountState (ls & lsCertStateL.certDStateL.dsIRewardsL .~ emptyInstantaneousRewards) ss nm & ...
```
Both pots reused byte-identical/unchanged on failure. `dsIRewardsL .~ emptyInstantaneousRewards` fires in BOTH branches (success and failure) — pending MIR state is always wiped at the boundary regardless of outcome. Insolvent MIR requests are silently and permanently discarded, never retried in a later epoch. No partial/proportional application exists anywhere in this path.

## Registered-credential filter — Map.intersection, applied at the MIR boundary (not at DELEG time)

`irwdR`/`irwdT` are computed via `Map.intersection` against the CURRENT (as-of-boundary) `accountsMap` BEFORE `totR`/`totT` are summed, in `Mir.hs` — not filtered earlier in `Deleg.hs`. Excluded (unregistered) entries never count toward totals, are never debited, never paid — they simply vanish when `dsIRewards` is wiped. Unlike ordinary RUPD leftover-reward routing (which explicitly redirects unclaimed to treasury), MIR does NOT route filtered-out entries anywhere — they're just discarded in place (money stays in the pot, never subtracted).

## Era status — Conway removes MIR structurally, not via a PV gate

- `hardforkAlonzoAllowMIRTransfer pv = pvMajor pv > natVersion @4` (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Era.hs`, PV5+/Alonzo) gates whether `SendToOppositePotMIR` and negative `StakeAddressesMIR` deltas are allowed at all — NOT a disable/removal gate. MIR certs remain fully live through Babbage.
- Conway removes MIR ENTIRELY, structurally: `eras/conway/impl/src/Cardano/Ledger/Conway/TxCert.hs` — `ConwayTxCert = ConwayTxCertDeleg | ConwayTxCertPool | ConwayTxCertGov`, no `MIRCert` constructor at all. `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/NewEpoch.hs`'s `newEpochTransition` goes `updateRewards → trans @(EraRule "EPOCH")` directly — the MIR call site is deleted, not routed to a no-op. `Conway.Era` still declares `type instance EraRule "MIR" ConwayEra = VoidEraRule "MIR" ConwayEra` only so generic rule-enumeration code typechecks; it is never invoked. Babbage still shares Shelley's `Mir.hs`/`Deleg.hs` verbatim.

## Rust port shape (dugite-ledger `apply_pending_mir`)

Haskell's function here is TOTAL/panic-free by construction (`Void` predicate failure). Correct translation:
1. Filter both pending maps (reserves-targeted, treasury-targeted) to currently-registered credentials FIRST.
2. Sum each filtered map → totR, totT.
3. Compute availableReserves = reserves +/- own delta only (no cross term); same for treasury.
4. One combined check: `totR <= availableReserves && totT <= availableTreasury`.
5. On failure: leave both pots byte-identical, still clear pending MIR maps, emit a no-op event (no error type needed) — never panic/Err.
6. On success: apply transfers, clear pending maps.
Use checked/saturating arithmetic for the pot +/- delta and pot - total steps (Haskell's unbounded Integer/Coin can't underflow; Rust u64/i64 can) — treat any would-be-negative as the insolvent branch rather than trapping.

Related: [[conway-certstate-encoding]] for InstantaneousRewards/DState CBOR shape (if applicable pre-Conway eras).
