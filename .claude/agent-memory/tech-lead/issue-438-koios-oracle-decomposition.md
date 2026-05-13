---
name: Issue 438 Koios oracle decomposition
description: Koios pool_history vs account_reward_history semantics for single-owner-pool leader rewards
type: reference
---

For a single-owner pool (owner credential == reward account credential):

- **Koios `pool_history.pool_fees`** = Haskell `leaderRew` = `cost + floor((R_pool − cost) × (m + (1 − m) × s/σ))`. This is ONLY the operator/leader payment.
- **Koios `pool_history.deleg_rewards`** = sum of `memberRew` for ALL delegators including the owner-as-member.
- **Koios `pool_history.member_rewards`** = `deleg_rewards − owner_as_member`. So `owner_as_member = deleg_rewards − member_rewards`.
- **Koios `account_reward_history.amount` (type=leader)** at epoch E = `pool_fees + owner_as_member` because both land on the same credential.

**Why:** When investigating reward divergences for single-owner pools, the *account* reward (e.g. 352,901,742) is NOT the leader-formula value — it's the sum of `leaderRew + memberRew(owner)`. Comparing dugite's per-credential output directly to `account_reward_history` is correct, but comparing to `pool_fees` will look like a much bigger gap because `pool_fees` excludes the owner-as-member share.

**How to apply:** When validating dugite's reward computation against Koios for any single-owner / heavily-pledged pool, always decompose:
```
expected_credential_total = pool_fees + (deleg_rewards − member_rewards)
```
For multi-owner pools, sum `(deleg_rewards − member_rewards)` proportionally over owners. For pools where owner credential ≠ reward account credential, the two payouts land on different credentials and `account_reward_history.amount` reports only one of them per row (with distinct `type` fields).

Source: cardano-ledger `eras/shelley/impl/src/Cardano/Ledger/Shelley/Rewards.hs` `rewardOnePool` — owners receive BOTH `leaderRew` (paid via `poolRA`) AND `memberRew` (paid via the delegator iteration; not skipped for owners).

Dugite at `crates/dugite-ledger/src/state/rewards.rs:373-377` *does* skip owners from the member loop and folds `(1-m)·s/σ·remainder` into `operator_reward` — which is mathematically equivalent for a single-owner pool where owner credential == reward account credential, since both terms credit the same key.
