---
name: Issue #438 PV10 reward discrepancy
description: 3505-lovelace leader-reward drift at preview epoch 1268, blocking PV10 withdrawal checks re-enable
type: project
---

## Issue
Preview testnet, account `bc636ae45...` (operator of `pool14rn9dq87dgj2z8g3lp4n0a78fewxff3gkgjkmz72ew44ym79xpp`), epoch 1268 leader reward.

- **Koios oracle**: 352,901,742 lovelace
- **Dugite**: 352,905,247 lovelace (+3,505)

The error propagates into the reward balance available at block 4197081 in epoch 1270; PV10 `testIncompleteAndMissingWithdrawals` rejects the withdrawal because amount != balance.

## Status
- Workaround in place: PV10 withdrawal checks removed from `apply_valid_tx` (commit 9a631979e). Mempool admission still enforces.
- Investigation commit 48de76b4: regression scaffold pinning Koios value as `#[ignore]` test in `crates/dugite-ledger/src/state/rewards.rs::test_koios_preview_epoch_1268_leader_reward_issue_438`.
- Issue left **OPEN** — root cause not isolated.

## Hypotheses (ranked)
1. **owner_stake in GO snapshot includes too much** — RUPD at boundary 1269→1270 reads `go.stake_distribution[owner_key]`. If dugite includes a reward balance Haskell hasn't yet applied at GO-capture time, owner stake is inflated ≈ 6.6M lovelace → 3,505 added to leader_extra via `0.95 × (Δowner/pool_stake) × (P - cost)`.
2. **owner credential type mishandling** — single-owner pool, owner_set excludes KeyHash; if treated as Script, owner falls through both branches.
3. **prev_protocol_params capture timing** — `epoch.rs:719-721` snapshots after PPUP. Subtle ρ/τ/a0/n_opt drift possible.

## Why: 
Re-enabling PV10 checks before root-cause is fixed will re-stall the node on preview.

## How to apply
- Do NOT revert 9a631979e until the `#[ignore]` regression test passes byte-equal.
- To reproduce: capture `go` snapshot fixture from a preview replay at boundary 1269→1270, query Haskell `cardano-cli query stake-snapshot --pool-id pool14rn9dq...` for the gold values, diff owner_stake first.

## Files
- `/Users/michaelfazio/Source/dugite/crates/dugite-ledger/src/state/rewards.rs` (`compute_reward_update`)
- `/Users/michaelfazio/Source/dugite/crates/dugite-ledger/src/state/epoch.rs:180-264` (SNAP build, ssStake equivalent)
- `/Users/michaelfazio/Source/dugite/crates/dugite-ledger/src/eras/conway.rs` (workaround site)
