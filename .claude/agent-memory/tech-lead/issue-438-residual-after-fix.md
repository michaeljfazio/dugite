---
name: Issue #438 residual after df2655330 fix
description: After the headline RUPD-on-empty-GO fix, dugite still has ~25-30K lovelace per-boundary pool_reward overshoot and ~2.67M ADA treasury drift at end of epoch 11. Per-epoch diff analysis localizes the residual.
type: project
---

After commit `df2655330` (fix: fire RUPD with empty GO snapshot to drain expansion + treasury cut), the original +0.27% systematic pool_reward overshoot is reduced ~67% but a residual persists.

**Why:** the residual is a separate smaller-magnitude bug. Investigation needed before re-landing the PV10 withdrawal check (#479) and closing #438.

**How to apply:**

1. **Per-boundary trajectory** (preview, dugite vs Koios treasury at end of epoch X):

   | Epoch X | dugite (lovelace) | Koios (lovelace) | diff (dugite - Koios) |
   |---:|---:|---:|---:|
   | 1 | 9,000,000,087,558 | 9,000,000,000,000 | +87,558 |
   | 2 | 17,994,600,129,087 | 17,994,600,087,558 | +41,529 |
   | 3 | 26,983,803,410,592 | 26,983,803,369,087 | +41,505 |
   | 4 | 36,217,203,091,357 | 35,967,613,128,648 | **+249,589,962,709** (+250K ADA) |
   | 5 | 45,461,981,008,040 | 45,195,372,193,123 | +266K ADA |
   | 6 | 54,722,399,879,375 | 54,434,502,740,001 | +288K ADA |
   | 7 | 63,801,152,067,252 | 63,689,264,884,685 | +112K ADA |
   | 8 | 72,537,908,671,360 | 72,762,471,510,999 | **−225K ADA (FLIPS NEGATIVE)** |
   | 9 | 79,465,353,038,988 | 81,493,891,671,936 | **−2.03M ADA** |
   | 10 | 85,515,378,282,283 | 88,477,859,079,030 | −2.96M ADA |
   | 11 | 91,856,506,777,161 | 94,524,229,963,155 | −2.67M ADA |

2. **Per-boundary delta in cumulative diff** (= per-boundary RUPD difference):

   | Boundary | Δ in cumulative diff |
   |---:|---:|
   | 3→4 | +209K ADA (dugite over-credited) |
   | 6→7 | −176K |
   | 7→8 | −337K |
   | **8→9** | **−1.8M ADA (HUGE single-boundary loss)** |
   | 9→10 | −934K |
   | 10→11 | +294K |

3. **Suspect: boundary 8→9 specifically**. At this boundary in preview, pool count jumped from 5 → 27 (22 new pools registered between epochs 7 and 9), and `Koios.deposits_stake` grew from 1.506M → 3.510M ADA (= ~1M new stake registrations × 2 ADA each). Dugite's per-boundary treasury+reserves accounting at this boundary diverges from Koios by 1.8M ADA. The MOST LIKELY mechanism: rewards distributed by RUPD to credentials that DEREGISTERED between when the GO snapshot was built (end of epoch 7) and when the RUPD applies (boundary 9→10) — Haskell's `frTotalUnregistered` forwards these forfeited rewards to treasury; dugite likely either (a) doesn't compute rewards for those creds because they're missing from current `reward_accounts`, or (b) computes them but routes them differently.

4. **Per-pool overshoot at the original target boundary 1269→1270**: 25,066 lovelace (down from 74,943 pre-fix). Back-derives to ~500K lovelace pool_reward overshoot, which corresponds to ~10K ADA excess in `reward_pot` at that boundary, which corresponds to ~6.83M ADA more reserves than Koios's (which is consistent with the cumulative −2.67M treasury drift × time-since-fix).

5. **Next step**: instrument `compute_reward_update` to emit per-cred reward computation outcomes (registered vs unregistered routing) at boundary 8→9 and compare to what Haskell would compute. The suspect path is the iteration over `go.delegations` where a delegator's `cred_hash` is checked against current `certs.reward_accounts.contains_key()` — if the cred deregistered between GO build and RUPD, this check returns false and the reward is forwarded to treasury. Verify this matches Haskell's `frTotalUnregistered` semantics exactly, including the credential-set used for the check.

6. **PV10 withdrawal-check re-land status (#479)**: BLOCKED. Residual ~25K lovelace per-boundary overshoot still accumulates in `reward_accounts[bc636ae45…]` over many epochs without withdrawal, eventually causing dugite_balance ≠ haskell_balance. Withdrawal txs signed for haskell_balance would still fail dugite's check. Wait until residual eliminated.
