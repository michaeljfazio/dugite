---
name: drep-distr-deposit-attribution
description: Exact composition of a DRep's (and governance-voting SPO's) stake weight in computeDRepDistr — which deposits count, which don't, and where each is keyed. Live-verified against IntersectMBO/cardano-ledger master 2026-07-26.
metadata:
  type: reference
---

Verified live via `gh api` against IntersectMBO/cardano-ledger master, 2026-07-26 (fetched `eras/conway/impl/src/Cardano/Ledger/Conway/Governance/DRepPulser.hs`, `.../Governance.hs`, `.../Governance/Proposals.hs`, `.../State/VState.hs`, `libs/cardano-ledger-core/src/Cardano/Ledger/DRep.hs`, `.../State/Account.hs`, `.../State/Stake.hs`). Triggered by a dugite `drep-stake-distribution` byte-exact divergence investigation (800,500 ADA gap = 8 live gov-action deposits @100k ADA sharing one return address + a coincidental 500 ADA).

## The function: `computeDRepDistr` (`DRepPulser.hs:200-241`)

```haskell
computeDRepDistr instantStake regDReps proposalDeposits poolDistr dRepDistr =
  Map.foldlWithKey' go (dRepDistr, poolDistr)
  where
    go (!drepAccum, !poolAccum) stakeCred accountState =
      let mInstantStake = Map.lookup stakeCred (instantStake ^. instantStakeCredentialsL)
          mProposalDeposit = Map.lookup stakeCred proposalDeposits
          stakeAndDeposits = fold $ mInstantStake <> mProposalDeposit
       in ( addToDRepDistr accountState stakeAndDeposits drepAccum
          , addToPoolDistr accountState mProposalDeposit poolAccum )
    addToPoolDistr accountState mProposalDeposit distr = fromMaybe distr $ do
      stakePool <- accountState ^. stakePoolDelegationAccountStateL
      proposalDeposit <- mProposalDeposit
      ... distr & poolDistrDistrL %~ Map.insert stakePool (ips & individualTotalPoolStakeL <>~ proposalDeposit)
             & poolDistrTotalL %~ ...
    addToDRepDistr accountState stakeAndDeposits distr = fromMaybe distr $ do
      dRep <- accountState ^. dRepDelegationAccountStateL
      let balance = accountState ^. balanceAccountStateL
          updatedDistr = Map.insertWith (<>) dRep (stakeAndDeposits <> balance) distr
      Just $ case dRep of
        DRepAlwaysAbstain -> updatedDistr
        DRepAlwaysNoConfidence -> updatedDistr
        DRepCredential cred | Map.member cred regDReps -> updatedDistr
                             | otherwise -> distr
```

It folds over `Accounts` (every registered stake credential), and per credential:
`total = InstantStake[cred] + ProposalDeposits[cred] + AccountBalance[cred]` (rewards), added to
whichever DRep that credential currently delegates to (only if `dRep` is `DRepAlwaysAbstain`,
`DRepAlwaysNoConfidence`, or a `DRepCredential` that is a member of `regDReps` — an unregistered
target DRep silently drops the contribution, `distr` unchanged). Same fold ALSO adds
`ProposalDeposits[cred]` (only, not InstantStake/balance — comment: "the rewards and stake are
already added to it by the SNAP rule") to the SPO's entry in `poolDistr` when that credential
delegates to a stake pool — this is a SEPARATE `PoolDistr` value used for governance SPO-vote
weight, not the leader-schedule/reward `PoolDistr` (see below).

## Inputs, and where each comes from (`Governance.hs:469-516`, `setFreshDRepPulsingState`)

- `instantStake = utxoState ^. instantStakeG` — UTxO-derived per-credential stake
  (`InstantStake`/`instantStakeCredentialsL`, `libs/cardano-ledger-core/.../State/Stake.hs:191`),
  maintained incrementally on every TxOut add/delete. Contains ONLY UTxO value, never rewards or
  deposits.
- `dpProposalDeposits = proposalsDeposits props` — `Proposals.hs:566-579`:
  ```haskell
  proposalsDeposits = F.foldl' (\m gas -> Map.insertWith addCompactCoin
      (gas ^. gasReturnAddrL . accountAddressCredentialL)
      (fromMaybe (CompactCoin 0) $ toCompact $ gas ^. gasDepositL) m) mempty . proposalsActions
  ```
  Folds over ALL currently-live `GovActionState`s (every proposal not yet enacted/expired/removed),
  keyed by the **RETURN ADDRESS's staking credential** (`gasReturnAddrL`), NOT the proposal
  submitter's payment credential. Deposits from multiple proposals sharing one return address are
  **summed** (`Map.insertWith addCompactCoin`) into that one credential's entry. This snapshot is
  frozen at pulser creation (start of the epoch) and does not change mid-epoch even if new proposals
  are submitted or old ones ratified within that same epoch (they land in the NEXT pulser).
- `balance = accountState ^. balanceAccountStateL` — the reward-account balance field of
  `AccountState` (`libs/cardano-ledger-core/.../State/Account.hs:98`), i.e. withdrawable rewards.
- `dpStakePoolDistr = stakePoolDistr` param, sourced by the NEWEPOCH caller from
  `ssStakeMarkPoolDistr` (the current-epoch "mark" snapshot pool distribution) — this is a
  **governance-only** derivative; the leader-schedule/reward `PoolDistr` used 2 epochs later for VRF
  comes from the independently-computed "go" snapshot and NEVER has proposal deposits added to it.

## What does NOT contribute to DRep or governance-SPO stake

- **Stake key registration deposit** (`ppKeyDeposit`) — lives in `AccountState.depositAccountStateL`
  (`Account.hs:100`), a field `computeDRepDistr` never reads (only `balanceAccountStateL` is read).
  Excluded.
- **DRep registration deposit** (`ppDRepDeposit`) — lives in `DRepState.drepDeposit`
  (`libs/cardano-ledger-core/src/Cardano/Ledger/DRep.hs:166-169`), inside `VState.vsDReps`, a
  completely separate credential namespace (`Credential DRepRole`) from `Accounts`
  (`Credential Staking`). `computeDRepDistr` only reads `dpDRepState`/`regDReps` for the
  `Map.member cred regDReps` registration-membership check — the deposit **value** itself is never
  added to anything in this function. Excluded. (Refund tracked separately via
  `lookupDepositVState`/`drepDepositL`, paid out at DRep deregistration.)
- **Pool registration deposit** (`ppPoolDeposit`) — tracked in `PState`, refunded via POOLREAP;
  entirely outside `computeDRepDistr`. Excluded from both DRep distr and governance pool distr (only
  *proposal* deposits get added to `poolDistr` here, not the pool's own registration deposit).

## Deposit refund transition (enacted/expired proposal)

`Epoch.hs:179-193` `returnProposalDeposits` (see [[conway-ratify-precision-facts]] #8) pays the
deposit into the return credential's `AccountState.balanceAccountStateL` via
`updateLookupAccountState` (or into treasury as `unclaimed` if the account is no longer registered).
Net effect on THAT credential's contribution to `computeDRepDistr` is neutral across the epoch
boundary (assuming the credential stays registered and delegated to the same DRep, and doesn't
withdraw in between): before, it was counted via `mProposalDeposit`; after, it's counted via
`balance`. It moves from the `dpProposalDeposits` bucket to the `balanceAccountStateL` bucket, not
in or out of the DRep's total.

## Consensus-relevant gotcha for a from-scratch (non-Haskell) port

A divergence of exactly `sum(live gov-action deposits sharing one return-address credential)` in a
`drep-stake-distribution` query is the signature of omitting `proposalsDeposits`/`dpProposalDeposits`
entirely from the DRep-distribution fold. A DRep's OWN `ppDRepDeposit` registration deposit should
**never** appear in this number per the code above — if a divergence's magnitude appears to include
it too, re-diff per-credential (not just the aggregate DRep total) before assuming the DRep's own
deposit is the cause; it is more likely a second, unrelated omission (e.g. that credential's own
`balanceAccountStateL` reward balance, or a stray key-deposit-vs-balance mixup on the port's side).

## Related
[[conway-ratify-precision-facts]] fact #4/#5-6 — `RatifyEnv` frozen fields, pulser creation ordering,
`dpProposalDeposits = proposalsDeposits props` (this file supersedes that one-line summary with full
source and the deposit-exclusion facts).
