---
name: project_state_2026_06_05
description: Post-PR-727 state assessment: open reward divergence bugs, fuzz CI status, mainnet epoch validation coverage
type: project
---

# Dugite State — 2026-06-05 (Post #727 merge)

**Why:** Tracks open bugs, validation coverage, and CI health immediately after the snapshot-backend-meta mega-merge.

**How to apply:** Use when assessing what to work on next. These bugs are NOT in the GH issue tracker (only #607 Peras is open).

---

## Branch State
- `main` is trunk. PR #727 merged 2026-06-05T14:16 UTC.
- `snapshot-backend-meta` branch is now merged. Clean working tree on main except untracked notes files.

## CI Status (as of 2026-06-05)
- CI (main): PASSING (24m50s after #727 merge).
- Nightly Benchmarks: PASSING.
- Fuzz Testing (last run 03:05 UTC, PRE-MERGE): 6 failures.
  - 5 are compile errors (`missing field raw_header_body` / `mismatched types`) from fuzz targets built against pre-#727 code. SELF-HEALING — next nightly will be clean (current code builds these targets fine).
  - 1 REAL crash: `fuzz_dugite_uplc_program_decode` (crash-f0ffcb568daff86abe9627cb093b17e7e167a053). CONFIRMED FIXED by #727 UPLC work — artifact no longer reproduces against current code.

## Byte-Exactness Validation Coverage (mainnet)
- ep208–238 (genesis → Allegra boundary): VALIDATED byte-exact via genesis replay + Koios /totals diff.
- ep239+ (Mary, Alonzo, Babbage, Conway): NOT YET VALIDATED on mainnet. Replay harness exists.

## Open Reward/Ledger Bugs (NOT filed as GH issues — tracked in untracked notes files)

### 1. Mainnet ep213 Residual (~10.19M compounding to -1.585T by ep233)
- File: `REWARD-DIVERGENCE-MAINNET-ep213.md`
- Primary fix (e853b7b10): operator-prefilter gates only operator credit, not whole pool.
- Residual: dugite over-distributes ~10.19M at ep213 for the 4 fixed pools. By ep233 this compounds to -1.585T reserves. The frTotalUnregistered routing is NOT the cause (already implemented correctly in epoch.rs:135-145). Hypotheses: owner_set incompleteness OR member-share rounding. Needs per-account instrumentation for the 4 pools' delegators vs Koios account_reward_history.
- Severity: Medium-High (compounds across Shelley era, but does not affect Conway; magnitude grows then shrinks as d-window exits).

### 2. Preprod ep57: UTxO Stake Asymmetry in Incremental Update
- File: `REWARD-DIVERGENCE-FINDINGS.md`
- Two delegators each −5 ADA in stake_distribution at ep55 snapshot (for ep57 rewards). The stake_routing add/spend paths are ASYMMETRIC: a spend-subtraction fires against a stake cred that was never (fully) add-credited. The saturating_sub clamp loses the stake silently. First clamp at tx b6ce541006fac5ac8b4a21e56a9c0515f9722f05011b496968121be43c2f02d7.
- Severity: High (compounds, causes ep181 WithdrawalAmountMismatch halt on from-genesis preprod sync).

### 3. Preprod ep292: LedgerDelta Rollback Corrupts reward_accounts
- File: `REWARD-TIP-DIVERGENCE-ep292.md`  
- Root-caused: apply_block_with_delta leaves reward_changes EMPTY (apply.rs:1428). On TriggeredFork, rollback_via_seq resets certs from the anchor (frozen at last snapshot), losing all reward credits and withdrawals in the snapshot→intersection window. The ep292 halt (withdrawal 45456692 vs dugite 109072477) is exactly explained: rewarded activity in the fork window was lost/duplicated.
- Fix design: either snapshot the imbl reward_accounts in each LedgerDelta (Option A — cheapest due to imbl structural-sharing), or construct RewardChange::Credit/Withdraw deltas. Must also handle delegations/pool_params/gov which are likely corrupted by the same mechanism.
- Severity: CRITICAL for preprod/mainnet correctness — any fork during live sync can corrupt reward balances.

### 4. CEK Budget/Error/unIData Residuals (Babbage Plutus phase-2, #22 follow-on)
- Described in `POST-HOLD-PLAN.md` section #22.
- ~398 budget mismatches, ~186 Error-class mismatches, ~44 unIData class on preprod Babbage/Alonzo window.
- Error class: a ScriptContext FIELD the validator inspects is still wrong — likely one field in script_context.rs / populate_v1_v2.rs.
- unIData class: a field encoded without `I n` wrapper — subtle schema bug.
- Requires fresh DUGITE_PHASE2_DUMP_DIR replay (May-31 dumps are stale/pre-fix).
- Severity: Medium (is_valid honored, non-fatal for replay; blocks preprod full-sync parity).

## PV10 Withdrawal Checks
- Re-enabled in commit 65c976bc4 (issue #479 closed). Verified ep1270-1279 no false rejections.

## UPLC Conformance
- SKIP_LIST is empty. 100% conformance as of #727.

## Only 1 Open GH Issue
- #607: Dijkstra Phase 1.4 peras_certificate — blocked on upstream Peras spec.
- All other bugs tracked in untracked notes files on working tree.

## Dependabot PRs Pending (open PRs)
- #716-#726: prost 0.13→0.14, tonic 0.12→0.13, serde_json, dashu-base, sysinfo, hyper, azure/helm, download-artifact bumps.
- Most are safe to merge individually but prost/tonic must be coordinated.
