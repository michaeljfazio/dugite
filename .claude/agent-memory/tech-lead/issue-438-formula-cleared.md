---
name: Issue #438 formula cleared by synthetic test 2026-05-13
description: dugite leader/member formula reproduces Haskell exactly given correct owner stake; bug isolated to snapshot/owner-stake construction
type: project
---

## Definitive negative result (2026-05-13)

Static investigation of all three suspects (credential type, leader formula
rounding, undistributed pot leftover) ruled out the formula itself.  Synthetic
test `test_issue_438_pool_1268_synthetic_leader_reward` in
`crates/dugite-ledger/src/state/rewards.rs` reproduces Haskell `leaderRew`
byte-exactly (352_901_742) using Koios-oracle inputs:

- pool_stake = 1_597_168_222_937
- owner_stake = 511_912_077  (from `account_stake_history(epoch_no=1268)`)
- R_pool = pool_fees + deleg_rewards = 596_472_990
- cost = 340_000_000, margin = 1/20, pledge = 0

Independent `memberRew(owner) = 78_092` also matches Koios decomposition
(`deleg_rewards − member_rewards = 243_649_340 − 243_571_248 = 78_092`).

## Back-calculated owner-stake inflation

From the 3505-lovelace overshoot:
- Δowner ≈ 22_980_000 lovelace ≈ 22.98 ADA over the Koios value.

This is what dugite's GO snapshot adds to `owner_stake_by_pool[pool14rn9dq…]`
relative to Haskell at the 1267→1268 boundary.

## Why
- Re-enabling PV10 withdrawal checks in block apply (`9a631979e` revert)
  remains blocked until the snapshot drift is fixed; the gold formula is
  not the culprit.

## How to apply
- Next step is **live capture**, not more static reasoning.  Replay preview to
  epoch 1267, dump `go.stake_distribution[owner_keyhash]` and
  `go.pool_stake[pool_id_h28]` for pool14rn9dq…, diff against Haskell
  `cardano-cli query stake-snapshot`.
- Do NOT rerun the formula audit — synthetic test pins it.
- Suspects to investigate live: stale reward balance in snapshot merge,
  pointer-stake leftover counted into pool_stake numerator while
  stake_distribution drops it (or vice versa), or duplicate-counted
  delegation between mark→set→go rotation.

## Files
- `crates/dugite-ledger/src/state/rewards.rs::tests::test_issue_438_pool_1268_synthetic_leader_reward` (passing)
- `crates/dugite-ledger/src/state/rewards.rs::compute_reward_update` (lines 253-414, formula verified clean)
- `crates/dugite-ledger/src/state/epoch.rs::process_epoch_transition` lines 180-264 (snapshot construction — primary suspect)
