---
name: Issue #438 Koios epoch_stake vs Haskell ssStake semantics
description: Koios account_stake_history.active_stake is UTxO-only; Haskell ssStake adds reward balance; reconciles synthetic test math
type: project
---

## Verified 2026-05-13

Koios `account_stake_history.active_stake` is populated from cardano-db-sync
`epoch_stake.amount`, which per the db-sync schema is **"the amount (in Lovelace)
being staked"** sourced from the SET snapshot — UTxO-delegated stake only,
**NOT including reward balance**.

Haskell ledger `ssStake` is built by `resolveActiveInstantStakeCredentials` ->
`addInstantActiveStakeWithDelegation` which combines `stake <> balance` per
credential (UTxO + reward balance), so the in-ledger stake input to `leaderRew`
and `memberRew` includes rewards.

## Why the synthetic test happens to be right

`test_issue_438_pool_1268_synthetic_leader_reward` passes `owner_stake =
511_912_077` (Koios value) and reproduces Koios's recorded leader credit
`352_901_742` byte-exactly. This is consistent ONLY if owner had **zero
reward balance** at the 1267→1268 boundary (or the rewards happened to be
withdrawn the same epoch they were earned).

That makes the 22.98 ADA dugite-vs-Haskell gap a **stale balance** in dugite's
`reward_accounts[owner]` at the 1267→1268 boundary — not a formula bug, not a
snapshot-construction bug, not a credential-type-collision bug.

## Where 22.98 ADA could leak in

Static suspects in dugite's reward-account mutation paths (ranked by
likelihood of accumulating an extra ~22 ADA over 600+ epochs):

1. **Withdrawal-on-tx-rollback**: tx with a `Withdrawals` map gets applied
   then rolled back; balance restoration code path in ledger_seq.rs around
   line 791-876.  If restoration doesn't reverse properly, the withdrawn
   amount remains visible while the tx never lands.
2. **Double-credit at epoch boundary** when an RUPD is computed AND a
   `pending_reward_update` was also queued — conway.rs:342 applies pending,
   conway.rs:361 applies fresh RUPD.  Could double-credit if both paths fire
   for the same epoch.
3. **PV9→PV10 boundary withdrawal-validation drift**: commit 9a631979e
   disabled PV10 withdrawal checks in apply_valid_tx; check whether mempool
   admission and apply paths agree on whether a withdrawal's amount matches
   the recorded balance, and whether mismatched withdrawals leave residual
   balance instead of clearing it.

## What blocks a static fix

Without a per-epoch snapshot diff of `reward_accounts[owner_keyhash]`
between dugite and a synced Haskell node, we cannot identify which of the
~600 epoch transitions inserted the extra 22.98 ADA.  The bug is
path-dependent and not visible in any single function.

## How to apply

- Do not "fix" snapshot/formula code; the formula reproduces Haskell
  byte-exactly when fed correct inputs.
- The path forward is the instrumented preview replay documented in the
  prior agent memory `issue-438-formula-cleared.md`: dump dugite's
  `reward_accounts[owner_keyhash]` at every epoch boundary during preview
  replay, diff against Haskell cardano-cli `query stake-snapshot`, find the
  first epoch where they diverge.  Then audit the reward-account mutations
  in that single epoch.
- `9a631979e` revert (re-enable PV10 withdrawal checks) stays blocked until
  the source-of-divergence epoch is found.
