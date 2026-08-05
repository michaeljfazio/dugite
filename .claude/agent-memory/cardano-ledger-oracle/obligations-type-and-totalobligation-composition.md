---
name: obligations-type-and-totalobligation-composition
description: Verbatim `Obligations` record (4 fields: oblStake/oblPool/oblDRep/oblProposal), `sumObligation`/`allObligations`/`totalObligation`, and the exact Shelley-vs-Conway split of which era contributes which field via `obligationCertState`/`obligationGovState`. No committee-member deposit category exists. Live-verified 2026-08-05 at IntersectMBO/cardano-ledger 4849c13d6f70e5ab46add9af6e0ec5c537b61f69 (master HEAD, committed 2026-08-04).
metadata:
  type: reference
---

Fetched live via `gh api repos/IntersectMBO/cardano-ledger/contents/...?ref=4849c13d6f70e5ab46add9af6e0ec5c537b61f69` (see [[kb-table-files-missing-use-live-github]] for method). Triggered by a dugite deposit/obligation byte-exactness audit.

## The type — `libs/cardano-ledger-core/src/Cardano/Ledger/State/CertState.hs:427-459`

```haskell
-- | A composite of all the Deposits the system is obligated to eventually pay back.
data Obligations = Obligations
  { oblStake :: !Coin
  , oblPool :: !Coin
  , oblDRep :: !Coin
  , oblProposal :: !Coin
  }
  deriving (Eq, Ord, Generic)

sumObligation :: Obligations -> Coin
sumObligation x = oblStake x <> oblPool x <> oblDRep x <> oblProposal x

instance Monoid Obligations where
  mempty = Obligations {oblStake = Coin 0, oblPool = Coin 0, oblDRep = Coin 0, oblProposal = Coin 0}
```

Exactly 4 fields. **No committee-member deposit category exists anywhere in this type or its
composition** — Constitutional Committee membership carries no deposit at all (members are seated
via the `UpdateCommittee` governance action, not a deposit-bearing certificate); confirmed by
exhaustiveness, not merely by absence of a field name.

## The composition function — `libs/cardano-ledger-core/src/Cardano/Ledger/State/Governance.hs:664-679`

```haskell
potEqualsObligation certState utxoSt = obligations == pot
  where
    obligations = totalObligation certState (utxoSt ^. utxosGovStateL)
    pot = utxoSt ^. utxosDepositedL

allObligations :: (EraGov era, EraCertState era) => CertState era -> GovState era -> Obligations
allObligations certState govState =
  obligationCertState certState <> obligationGovState govState

totalObligation :: (EraGov era, EraCertState era) => CertState era -> GovState era -> Coin
totalObligation certState govState = sumObligation (allObligations certState govState)
```

`utxosDeposited` (the `UTxOState` field, the actual on-chain "deposit pot" Coin) is set to
`totalObligation certState govState` — recomputed fresh each epoch boundary, not incrementally
tracked (see below).

## Per-era split of which side (`CertState` vs `GovState`) contributes which field

**`obligationCertState`** (class method, `EraCertState`):
- Shelley (`eras/shelley/impl/src/Cardano/Ledger/Shelley/State/CertState.hs:77-84`,
  `shelleyObligationCertState`):
  ```haskell
  Obligations
    { oblStake = sumDepositsAccounts (certState ^. certDStateL . accountsL)
    , oblPool = fromCompact $ F.foldMap' spsDeposit (certState ^. certPStateL . psStakePoolsL)
    , oblDRep = Coin 0
    , oblProposal = Coin 0
    }
  ```
- Conway (`eras/conway/impl/src/Cardano/Ledger/Conway/State/CertState.hs:111-116`,
  `conwayObligationCertState`) — reuses Shelley's stake+pool computation verbatim, adds DRep:
  ```haskell
  conwayObligationCertState certState =
    let accum ans drepState = ans <> drepDeposit drepState
     in (shelleyObligationCertState certState)
          { oblDRep = fromCompact $ F.foldl' accum mempty (certState ^. certVStateL . vsDRepsL)
          }
  ```
  `oblStake` = sum of every registered `Accounts` entry's deposit
  (`AccountState.depositAccountStateL`, i.e. `ppKeyDeposit` at registration time, per-account, not
  `count × currentPParams.keyDeposit`). `oblPool` = sum of every live `StakePoolState`'s own
  `spsDeposit` (deposit value AT REGISTRATION, same "recompute from per-entity stored value, not
  count × current PParam" pattern — protects both against a mid-epoch `keyDeposit`/`poolDeposit` PPU
  changing the obligation of already-registered entities). `oblDRep` = sum of every live
  `VState.vsDReps` entry's `drepDeposit` (deposit at DRep registration time). Conway's `oblProposal`
  stays `Coin 0` here — it comes from the OTHER side, `obligationGovState`.

**`obligationGovState`** (class method, `EraGov`):
- Shelley/pre-Conway eras: no governance-action deposits exist; not applicable (`GovState` for those
  eras carries no Conway-style proposals).
- Conway (`eras/conway/impl/src/Cardano/Ledger/Conway/Governance.hs:414-420`):
  ```haskell
  obligationGovState st =
    Obligations
      { oblProposal = foldMap' gasDeposit $ proposalsActions (st ^. cgsProposalsL)
      , oblDRep = Coin 0
      , oblStake = Coin 0
      , oblPool = Coin 0
      }
  ```
  Sums `gasDeposit` over every `GovActionState` currently live in `cgsProposalsL` — i.e. every
  proposal that has NOT yet been removed (enacted / expired / pruned-as-sibling-of-an-enactment). The
  proposal deposit and the `Proposals` OMap are exactly one-to-one; there is no separate deposit
  ledger to drift out of sync with the proposal set itself.

Net: for Conway, `allObligations` = `conwayObligationCertState certState <> conwayObligationGovState
govState`, and the `Monoid`/`Semigroup Obligations` instance sums field-by-field, so the final
4-field breakdown is `oblStake` (CertState/DState/Accounts), `oblPool` (CertState/PState),
`oblDRep` (CertState/VState), `oblProposal` (GovState/Proposals) — each field sourced from exactly
one side, never double-contributed.

## Where `totalObligation` gets written into the live `utxosDeposited` pot

`eras/conway/impl/src/Cardano/Ledger/Conway/Rules/Epoch.hs`, end of `epochTransition`, AFTER
POOLREAP refunds, `applyEnactedWithdrawals`, `proposalsApplyEnactment`, and
`returnProposalDeposits` have all already mutated `certState2`/`govState1`:

```haskell
utxoState2 =
  utxoState1
    & utxosDepositedL .~ totalObligation certState2 govState1
    & utxosDonationL .~ zero
    & utxosGovStateL .~ govState1
```

This is a **full recomputation from the post-boundary state**, not an incremental delta — so
`utxosDeposited` is guaranteed to equal `sumObligation` of the actual live registries at every
epoch boundary by construction, not merely by an invariant that could drift. (`potEqualsObligation`,
shown above, is the property-test/assertion form of this same equality, used in conformance specs.)

## Dugite alignment (audited 2026-08-05, `crates/dugite-ledger/src/state/epoch_state_debug.rs:690-750`)

dugite's debug dump computes `deposits_stake` (= Haskell's `oblStake + oblPool` combined, matching
what `cardano-cli debug log-epoch-state` displays as one combined number),
`deposits_drep` (sum of `state.gov.governance.dreps[*].deposit`), and `deposits_proposal` (sum of
`state.gov.governance.proposals[*].procedure.deposit`) — a 3-way grouping of the same 4 underlying
fields (stake+pool merged for display only). No divergence found against this verbatim source in
this pass.

## Related
[[feedback_proposal_deposit_epoch_boundary]], [[conway-ratify-precision-facts]] #8,
[[conway-gov-enactment-effects-and-timing]] §4, [[drep-distr-deposit-attribution]] — all cover
`returnProposalDeposits`/deposit-refund timing, which is the DYNAMIC (per-epoch-boundary) half of
this picture; this file covers the STATIC composition (`Obligations` type itself and what feeds each
field), which none of those fully quoted verbatim before this session.
