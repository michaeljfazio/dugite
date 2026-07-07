---
name: mir-pot-transfer-semantics
description: MIR (Move Instantaneous Rewards) two-phase accumulate/apply design, exact delta/solvency formulas, registered-credential filter, non-throwing insolvency branch, and Conway removal
type: reference
---

## Two-phase design

1. **Accumulation (mid-block, cert-processing time)**: `Cardano.Ledger.Shelley.Rules.Deleg`
   (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs`), function `delegationTransition`
   → `updateReservesAndTreasury`. Handles `MIRCert targetPot mirTarget` TxCerts, writes into
   `DState.dsIRewards :: InstantaneousRewards`. STS instance constrained `AtMostEra "Babbage" era`
   — this module (and thus MIR-cert acceptance) is shared by Shelley/Allegra/Mary/Alonzo/Babbage.
   Babbage's own `Rules/Deleg.hs` is empty boilerplate that just wires
   `type instance EraRuleFailure "DELEG" BabbageEra = Shelley.ShelleyDelegPredFailure BabbageEra` —
   confirms Babbage reuses this exact rule verbatim.

2. **Application (epoch boundary)**: standalone `MIR` STS rule,
   `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Mir.hs`, function `mirTransition`.
   Called from `Cardano.Ledger.Shelley.Rules.NewEpoch.newEpochTransition`
   (`eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/NewEpoch.hs`) in this exact order:
   `updateRewards` (RUPD completion) → `trans @(EraRule "MIR")` → `trans @(EraRule "EPOCH")`
   (EPOCH internally does SNAP/POOLREAP/UPEC). So MIR sees pot balances **after** that epoch's
   monetary-expansion (RUPD) has already been applied.

## InstantaneousRewards type (libs/cardano-ledger-core/src/Cardano/Ledger/State/CertState.hs)

```haskell
data InstantaneousRewards = InstantaneousRewards
  { iRReserves :: !(Map (Credential Staking) Coin)
  , iRTreasury :: !(Map (Credential Staking) Coin)
  , deltaReserves :: !DeltaCoin
  , deltaTreasury :: !DeltaCoin
  }
-- invariant: deltaReserves + deltaTreasury = 0
```
Field is `DState.dsIRewards`. `SendToOppositePotMIR coin` (in Deleg.hs) sets deltaReserves/deltaTreasury
with OPPOSITE signs on the two fields (e.g. targetPot=ReservesMIR means "pay FROM reserves":
`iRDeltaReservesL <>~ invert(toDeltaCoin coin)`, `iRDeltaTreasuryL <>~ toDeltaCoin coin`).

## Exact epoch-boundary formula (Mir.hs `mirTransition`) — LOAD BEARING, own-pot-name only

```haskell
accountsMap = ds ^. accountsL . accountsMapL          -- CURRENT (at boundary) registered accounts
irwdR = iRReserves (dsIRewards ds) `Map.intersection` accountsMap
irwdT = iRTreasury (dsIRewards ds) `Map.intersection` accountsMap
totR = fold irwdR
totT = fold irwdT
availableReserves = reserves `addDeltaCoin` deltaReserves (dsIRewards ds)
availableTreasury = treasury `addDeltaCoin` deltaTreasury (dsIRewards ds)
update = Map.unionWith (<>) irwdR irwdT

if totR <= availableReserves && totT <= availableTreasury
  then -- APPLY: casReserves = availableReserves <-> totR
       --        casTreasury = availableTreasury <-> totT
       --        accounts += update (compactCoinOrError, summed if a cred is in both maps)
       --        dsIRewardsL .~ emptyInstantaneousRewards
  else -- NO-OP: chainAccountState UNCHANGED (both pots), only
       --        dsIRewardsL .~ emptyInstantaneousRewards  (still wiped!)
```

**CORRECTION to the naive guess `available = pot + oppositeDelta - ownDelta`**: it is NOT
cross-swapped. `availableReserves` uses `deltaReserves` (own name), `availableTreasury` uses
`deltaTreasury` (own name). The cross-pot-transfer semantics are already baked into the *sign*
of each field at accumulation time (Deleg.hs), not into which field the epoch-boundary code reads.

## Solvency check is a SINGLE combined AND across both pots — not independent per pot

`if totR <= availableReserves && totT <= availableTreasury` — one boolean gate. If EITHER pot
would be insolvent, NEITHER pot's transfer/payout happens (both `chainAccountState` fields stay
byte-identical to their pre-MIR values). This is stricter than "per-pot atomic": a treasury
shortfall also blocks an otherwise-solvent reserves payout in the same epoch.

**`PredicateFailure (MIR era) = Void`** and **`PredicateFailure (NEWEPOCH era) = Void`** — this
insolvency branch is NOT an STS failure / cannot throw. It's a pure `if/then/else` producing
either `MirTransfer` or `NoMirTransfer InstantaneousRewards Coin Coin` (event only, informational).
Translate to Rust as an `if`/branch returning unchanged state, never a `Result::Err`, and
definitely never a panic — the Haskell type system statically guarantees this code path cannot fail.

## On insolvency: dropped, not retried, no partial application

`dsIRewardsL .~ emptyInstantaneousRewards` appears in **both** branches (success and failure) of
`mirTransition`. So regardless of outcome, the pending MIR state is always wiped at that epoch
boundary — insolvent MIR requests are silently and permanently discarded, never retried in a
later epoch. No partial/proportional application exists anywhere in this code.

## Unregistered-credential handling: forfeited, not refunded to pot

`Map.intersection` filters `iRReserves`/`iRTreasury` down to `irwdR`/`irwdT` (currently-registered
only) **before** `totR`/`totT` are computed, and this happens only in `Mir.hs` at epoch-boundary
time — using accounts-registration state AS OF THE BOUNDARY (not as of cert-submission time). The
excluded (unregistered) entries are never summed into `totR`/`totT`, so they are never debited
from the pot AND never paid out — they simply vanish when `dsIRewards` is wiped afterward. Compare
this to `applyRUpdFiltered`'s ordinary-reward unregistered-credential handling elsewhere, which
routes unregistered reward amounts to treasury — MIR does NOT do that; MIR's unregistered
leftovers just disappear (implicitly remain in the pot since they were never subtracted).

## DELEG-time (cert submission, mid-block) check is SEPARATE and unfiltered — real predicate failure

`updateReservesAndTreasury` in Deleg.hs (called synchronously when a `MIRCert` is processed inside
a transaction):
```haskell
requiredForRewards = fold combinedMap   -- UNFILTERED: no registered-credential intersection here
requiredForRewards <= available
  ?! InsufficientForInstantaneousRewardsDELEG targetPot (Mismatch requiredForRewards available)
```
where (pre-Alonzo) `available = potAmount` unchanged, or (Alonzo+, `hardforkAlonzoAllowMIRTransfer`)
`available = potAmount \`addDeltaCoin\` delta`. This DOES throw a real predicate failure
(`ShelleyDelegPredFailure`, rejects the block) if violated — contrast with the epoch-boundary
check above, which never throws. Related DELEG-time-only failures in the same module: `MIRCertificateTooLateinEpochDELEG`
(cert submitted after `tooLate = firstSlotNextEpoch - stabilityWindow`), `MIRTransferNotCurrentlyAllowed`
/ `MIRNegativesNotCurrentlyAllowed` (pre-Alonzo gate), `MIRProducesNegativeUpdate`, `MIRNegativeTransfer`,
`InsufficientForTransferDELEG` (SendToOppositePotMIR-specific, checked against `availableAfterMIR`
in `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/NewEpochState.hs`:
`availableAfterMIR ReservesMIR as ir = casReserves as \`addDeltaCoin\` deltaReserves ir <-> fold (iRReserves ir)`,
symmetric for TreasuryMIR — same own-pot-name-only formula as above, note the EXTRA `<-> fold iR*`
term that Mir.hs's `availableReserves`/`availableTreasury` do NOT have, since Mir.hs subtracts
`totR`/`totT` in a separate step after the AND-check).

## Alonzo hardfork gate (NOT the Conway removal — do not conflate)

`eras/shelley/impl/src/Cardano/Ledger/Shelley/Era.hs`:
```haskell
hardforkAlonzoAllowMIRTransfer :: ProtVer -> Bool
hardforkAlonzoAllowMIRTransfer pv = pvMajor pv > natVersion @4   -- PV5+ (Alonzo onward)
```
Pre-Alonzo (PV2-4: Shelley/Allegra/Mary): `SendToOppositePotMIR` always fails
(`MIRTransferNotCurrentlyAllowed`); `StakeAddressesMIR` entries must all be non-negative
(`MIRNegativesNotCurrentlyAllowed`) and are plain `Map.union`'d (last-cert-wins per key), not
delta-accumulated. Alonzo onward (PV5+): pot-to-pot transfers allowed, and StakeAddressesMIR
entries may be negative as long as the unioned-with-`<>` running total per credential stays >= 0.

## Conway: MIR fully removed from the pipeline (not merely "no-op")

- `eras/conway/impl/src/Cardano/Ledger/Conway/TxCert.hs`: `ConwayTxCert era = ConwayTxCertDeleg
  !ConwayDelegCert | ConwayTxCertPool !... | ConwayTxCertGov !...` — **no MIRCert constructor at
  all**. No transaction can ever create a new pending MIR entry from Conway onward.
- `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/NewEpoch.hs` `newEpochTransition`: sequence is
  `updateRewards` (RUPD) → `trans @(EraRule "EPOCH")` directly. **No call to the MIR rule at all**
  — the call site itself was deleted, this isn't a void-rule no-op invocation.
  Conway also has its own `Rules/Deleg.hs` (`Cardano.Ledger.Conway.Rules.Deleg`) which only
  handles `ConwayDelegCert` variants — no MIRCert case exists to even pattern-match.
- `eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs` still declares
  `type instance EraRule "MIR" ConwayEra = VoidEraRule "MIR" ConwayEra` (+ Failure/Event) — present
  only so generic/shared code that enumerates all `EraRule` names by name still typechecks; it is
  never invoked by any real transition path in Conway.
- Net effect: Babbage→Conway HFC translation may still carry over a leftover `dsIRewards` value
  from the last Babbage epoch (CertState/DState is untouched by era translation), but since Conway
  never calls MIR again, that leftover (if ever non-empty, which would already be unusual since
  Babbage's own NEWEPOCH wipes it every epoch boundary) is simply inert dead data from Conway
  onward — nothing in Conway ever reads or clears `dsIRewards` again. In practice it should always
  be `def` (empty) by the time Conway starts, since the Babbage-side MIR rule wipes it every epoch.

## File index
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Mir.hs` — epoch-boundary MIR STS (Shelley..Babbage)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/Deleg.hs` — cert-time accumulation + DELEG predicate failures (Shelley..Babbage, `AtMostEra "Babbage"`)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rules/NewEpoch.hs` — Shelley..Babbage NEWEPOCH call site (RUPD→MIR→EPOCH)
- `eras/conway/impl/src/Cardano/Ledger/Conway/Rules/NewEpoch.hs` — Conway NEWEPOCH call site (RUPD→EPOCH, no MIR)
- `eras/conway/impl/src/Cardano/Ledger/Conway/Era.hs` — `EraRule "MIR" ConwayEra = VoidEraRule` (dead type family entry)
- `eras/conway/impl/src/Cardano/Ledger/Conway/TxCert.hs` — `ConwayTxCert` union, no MIRCert
- `eras/babbage/impl/src/Cardano/Ledger/Babbage/Rules/Deleg.hs` — empty wiring, proves Babbage reuses Shelley's Deleg/Mir
- `libs/cardano-ledger-core/src/Cardano/Ledger/State/CertState.hs` — `InstantaneousRewards` type + lenses (`iRReservesL`, `iRTreasuryL`, `iRDeltaReservesL`, `iRDeltaTreasuryL`, `dsIRewardsL`)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/LedgerState/NewEpochState.hs` — `availableAfterMIR` (used only for the DELEG-time SendToOppositePotMIR check, has an extra `<-> fold iR*` term vs. Mir.hs's own `available*`)
- `eras/shelley/impl/src/Cardano/Ledger/Shelley/Era.hs` — `hardforkAlonzoAllowMIRTransfer pv = pvMajor pv > natVersion @4`

Related: [[newepoch-ordering-details]] (general NEWEPOCH step order), [[reward-update-accounting]] (RUPD/applyRUpd, which runs immediately before MIR).
