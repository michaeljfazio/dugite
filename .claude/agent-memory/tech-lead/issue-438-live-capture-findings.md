---
name: Issue #438 live-capture findings 2026-05-13
description: Live preview replay through epoch 1273 with #471 instrumentation reveals the bug is systemic ~0.25% pool_reward overshoot per boundary, NOT a path-dependent stake inflation
type: project
---

Ran the reward-debug-dump instrumentation (commit `4eb0f5686`, feature `reward-debug-dump`) via `dugite-node dump-snapshot` over the existing `db-preview` ImmutableDB (4.2M blocks, 280 seconds). Captured per-boundary JSON for pool14rn9dq from genesis through epoch 1273. Cross-referenced against Koios `account_reward_history`.

**Why this matters:** the static-audit memo claimed the bug was a path-dependent +22.98 ADA inflation in `reward_accounts[owner]` accumulating across a specific block sequence. **This claim is now disproven by live data.**

**How to apply:**

1. The actual bug is a **systemic ~0.25% over-credit in `pool_reward`** at every boundary the pool earns rewards. 28/28 boundaries with Koios cross-reference show positive overshoot, no negatives, no zeros.

2. The dugite-vs-Koios delta in the owner's `go.stake_distribution` at the supposed bug boundary 1269→1270 is only **+89,872 lovelace** (≈ 0.09 ADA), NOT +22.98 ADA. The static audit's 22.98 ADA estimate was a back-calc from the leader_credit overshoot under the assumption that the bug was in `owner_stake`. That assumption was wrong.

3. The 89,872 lovelace stake delta accounts for only ~14 lovelace of the leader_credit overshoot. The remaining ~74,929 lovelace overshoot comes from a ~1.49M lovelace overshoot in `pool_reward` itself.

4. Pool_reward formula (`floor(perf × max_pool)`) inputs all verified against Koios at boundary 1269→1270:
   - blocks_made = 5 ✓, total_blocks = 2636 ✓, ss_fee = 1,400,829,565 ✓
   - pool_active_stake (1,599,479,099,666) = UTxO sum + reward-balance sum, matches expected
   - reserves derives correctly from genesis init `MAX − 30B ADA byron_initial_funds = 15B ADA` and per-boundary RUPD drains

5. **Bug is in dugite's `compute_reward_update` arithmetic** (`crates/dugite-ledger/src/state/rewards.rs:136-438`). Cardano-ledger oracle confirmed the formula STRUCTURE matches Haskell. So the divergence is in arithmetic detail — likely integer/rational handling somewhere in:
   - `expected_blocks` computation (line 175-185)
   - `expansion` calculation (line 166-194)
   - `treasury_cut` / `reward_pot` derivation (line 196-217)
   - `max_pool` factor1/factor2 (line 304-315)

6. **The dugite formula passes the synthetic test** (`test_issue_438_pool_1268_synthetic_leader_reward`) because that test bypasses `compute_reward_update` entirely and hardcodes R_pool = 596,472,990 from Koios oracle. Then dugite computes the leader-split byte-exactly. So the test correctly proves the SPLIT formula is right while the BUG is in the upstream R_pool computation that the test doesn't exercise.

7. **What it explains:** the long-period reward_accounts drift the static audit observed is real — but it's because each per-epoch over-credit (50-150K lovelace, larger in epochs where pool_reward < cost) goes INTO `reward_accounts[owner]` and accumulates between withdrawals. With no withdrawals between epochs 884 and 1208 (~324-epoch gap visible in account_updates), the cumulative excess can reach 22+ ADA in `reward_accounts` — but that's a SYMPTOM of the per-boundary bug, not the bug itself.

8. **Next session pickup point** — line-by-line audit of dugite's `compute_reward_update` against Haskell `Rewards.poolRewardInfo` + `Rules.Rupd.createRUpd`. Use cardano-haskell-oracle for the live Haskell source. Empirical signature: at boundary 1269→1270, dugite's pool_reward is +1.49M lovelace (~0.25%) above Haskell's. At boundary 11→12 (preview), dugite's pool_reward is +2.03M lovelace (~0.27%) above Haskell's. The percentage is roughly consistent but the absolute delta shrinks as reserves shrink and σ grows — exact relationship pending the precise formula identification.

9. **Reproduction:**

```bash
cargo build --release -p dugite-node --features dugite-ledger/reward-debug-dump
mkdir -p reward-dumps-issue-438
DUGITE_REWARD_DEBUG_DUMP=$(pwd)/reward-dumps-issue-438 \
DUGITE_REWARD_DEBUG_POOL_FILTER=a8e65680fe6a24a11d11f86b37f7c74e5c64a628b2256d8bcacbab52 \
./target/release/dugite-node dump-snapshot \
  --config config/preview-config.json --database-path ./db-preview \
  --stop-slot 110000000 --output /dev/null \
  --log-level info --log-output stdout
python3 scripts/issue_438_analyze_dumps.py /tmp/koios_rewards.json
```

10. **What's done and committed:**
   - `4eb0f5686` feat(ledger): per-boundary reward-debug dump (#471)
   - `efb74d212` chore(scripts): per-boundary reward-diff analyzer
   - `94c6a5bc4` chore: ignore reward-dumps-issue-438 capture dir

## RESOLVED 2026-05-13

Root cause identified and fixed. The bug was NOT in the pool_reward split formula or any per-pool arithmetic — it was in the final accounting step of `compute_reward_update`: `undistributed = reward_pot - total_distributed` was computed but never used. Fix: add `undistributed` to both `delta_treasury` and `delta_reserves`. Conservation identity now holds: `expansion + epoch_fees = delta_treasury + Σ(rupd.rewards)`.

**Commits:** `2a14be2fe` (fix rewards.rs + remove #[ignore]), `30fd58db8` (test updates), `a7591523b` (re-enable PV10 checks).

**Verification:** `test_koios_preview_epoch_1268_leader_reward_issue_438` passes with `rupd_credit = 352,901_742` matching Koios exactly. 4659/4659 workspace tests pass.

**Issues closed:** #438 and #479.
