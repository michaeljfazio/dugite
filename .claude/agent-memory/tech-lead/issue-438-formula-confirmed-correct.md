---
name: Issue #438 formula confirmed correct — divergence is reserve state drift
description: Per-pool pool_reward formula and split are byte-exact given matching inputs; +25,066 lovelace overshoot is 100% attributable to +4.887T lovelace excess reserves vs Haskell at epoch 1269
type: project
---

## Confirmed 2026-05-14

After adding `eprintln!` instrumentation to `compute_reward_update` and running a full preview replay:

**Actual runtime values at boundary 1269→1270:**
- `pool_stake = 1,597,934,031,603`, `self_delegated = 511,942,136`
- `total_active_stake = 1,259,466,359,695,472`, `total_stake = 36,830,736,289,297,439`
- `max_pool = 399,302,095`, `pool_reward = 596,971,933`
- `operator_reward = 352,926,808` (matches dump exactly)

**Formula is correct.** Using Koios oracle inputs (`pool_stake = 1,597,168,222,937`, `total_active_stake = 1,259,333,994,152,147`, Koios `reserves = 8,164,376,631,686,011`) the formula produces `operator_reward = 352,901,742` — **byte-exact match** with Koios target. Diff = 0.

**Root cause of the +25,066 lovelace overshoot:**
Dugite has `reserves = 8,169,263,710,702,561` vs Koios `reserves = 8,164,376,631,686,011` at epoch 1269.
Excess reserves = +4,887,079,016,550 lovelace (+4.887T lovelace, ~4.887 billion ADA).

This +4.887T reserve excess causes:
- Larger `expansion` (+8,946,069,644 lovelace)
- Larger `reward_pot` (+7,156,855,716)
- Larger `max_pool` (+483,021)
- Larger `pool_reward` (+498,292)
- Larger `operator_reward` (+25,066)

**What does NOT cause the divergence:**
- The perf formula (uses `total_active_stake/pool_stake` correctly as sigmaA)
- The max_pool formula (uses `pool_stake/total_stake` correctly for sigma)
- The operator/member split formula (correct)
- The eta computation (matches Haskell using integer expected_blocks and Fraction arithmetic)
- The snapshot stake values (differ only by 765K pool_stake, contributing only ~3K of the 25K)

**Reserve drift origins (multi-epoch accumulation):**
1. Genesis fee offset: epoch 0's fees (~437,790 lovelace) are included in ss_fee at boundary 0→1 by dugite but Haskell uses ss_fee=0 at genesis boundary. Contributes +350,232 lovelace to reserves at epoch 1. Compounds to ~443M at epoch 1269.
2. Larger divergences at boundaries 3→4 (+209K ADA) and 8→9 (+1.8M ADA) from the residual-after-fix memory — these are the dominant contributors.
3. The boundary 3→4 jump: treasury is +249,589,962,709 lovelace above Koios — this means ~250K ADA went to treasury in dugite that Haskell put into reward_accounts, causing reserves to UNDERRUN slightly at that boundary (dugite took more from reserves = less reserves). Wait — actually if treasury is higher, that means dugite OVER-credited treasury, which means dugite's reserves decreased MORE (→ reserves lower), not more.

**Why:** `delta_reserves = treasury_cut + total_distributed - fees`. If treasury_cut is larger, delta_reserves is larger, reserves decreases faster. So excess treasury in dugite = deficit in reserves.

At epoch 1269: treasury_excess + reserve_excess = ? Both are measured vs Koios. If dugite has MORE treasury (from frTotalUnregistered over-routing) AND MORE reserves (from under-distributing), these partially cancel. The net +4.887T reserve excess suggests dugite is under-distributing (credits fewer lovelace to reward_accounts, leaving more in reserves via smaller delta_reserves).

**Not yet traced:** exact boundary-by-boundary source of the +4.887T. The large jumps at 3→4 and 8→9 in the residual memory are the primary candidates.

**How to apply:**
- Do NOT attempt to fix the formula — it is correct.
- The fix path is finding and correcting the reserve accounting divergence (likely in frTotalUnregistered routing or deposit/refund handling at early epoch boundaries).
- The `test_issue_438_pool_1268_synthetic_leader_reward` test correctly validates the formula — keep it.
- Issue #479 (PV10 withdrawal check) remains blocked until reserves match.
