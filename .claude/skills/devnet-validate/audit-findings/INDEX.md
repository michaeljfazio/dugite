# Audit findings index

Status summary of every findings document. **Read this first** to see
what's resolved, what's in-progress, and the resolving commit for each.

| Document | Status | Resolving commit(s) | Notes |
|---|---|---|---|
| [`2026-05-28-skill-self-audit.md`](./2026-05-28-skill-self-audit.md) | **RESOLVED** | A1-A4 (`bd9b8e6` etc); A8 PeerCooling (`89170444f`, `46a5261c6`) | Two A8 divergences (peer-counter overlap, hot/warm/cold dedup) were false-positives per cardano-haskell-oracle re-verification — dugite was already Haskell-faithful |
| [`2026-05-28-round1-retry.md`](./2026-05-28-round1-retry.md) | **RESOLVED** | `0821af5d2` (last Round 1 attempt PASS) | 4 P0 mempool-admission gaps fixed: TimeTranslationPastHorizon, WithdrawalsNotInRewardsCERTS, MissingVKeyWitness for voters, MissingVKeyWitness for collateral |
| [`2026-05-28-round2-rupd-divergence.md`](./2026-05-28-round2-rupd-divergence.md) | **RESOLVED** | `037c464ea` | Conway-from-genesis RUPD: `prev_d=1/1` default + snapshot pre-fill caused 3.6T divergence at boundary 0→1 and 22 ADA at 1→2. Fix scoped to Conway-from-genesis init |
| [`2026-05-28-conway-ledger-predicate-audit.md`](./2026-05-28-conway-ledger-predicate-audit.md) | **RESOLVED** | `9222c9387` (DRep.4) + `9ee6164d9` (Babbage.2/3) + `19c86570e` (POOL.3 + WrongNetworkPOOL + GOV.13/17 + Shelley.8) | All 42 originally-🔍 entries closed: 24 → ✅ (already implemented), 13 → ✅ (newly implemented), 6 deprecated/non-Conway → ✅ (verified N/A). 0 ❌ remaining |
| [`2026-05-28-p3-residuals.md`](./2026-05-28-p3-residuals.md) | **PARTIAL** | `verify.sh` SKIP track (`b4365c96d`); multi-BP attribution `LD_P2_SMALL_STAKE_POOL2` env var (latest) | One residual remains: 22.14B-lovelace reserves diff at boundary 2→3 — needs Haskell `cardano-cli debug log-epoch-state` dump to root-cause (estimated 3-5h) |
| [`2026-05-28-session-summary.md`](./2026-05-28-session-summary.md) | **META** | `6bd42da17` | High-level session summary; lists all 6 P0 + 1 P1 + N P2 fixes |

## What's truly open after this session

Only **one** open item: the 22.14B-lovelace reserves divergence at boundary 2→3 on the Conway-from-genesis devnet.

- Treasury matches Haskell byte-exact at boundary 2→3 (7,197,832,802,160 each)
- Reserves under-deduct by 22,140,531,700 lovelace (~22 K ADA, 0.0004% of pot)
- Resolution requires capturing Haskell's `cardano-cli debug log-epoch-state` dump at boundary 2→3 of a Conway-from-genesis devnet sync (cardano-node 11.0.1) and diffing `_rewardUpdate.deltaR.unCoin` + `_rewardUpdate.rs` against dugite's `compute_reward_update` outputs at the same boundary
- This residual does NOT affect any Round 1/2/3 PASS criterion (Round 2 covers boundaries 0→1 and 1→2 which are byte-exact)
- Filed as P3 in `2026-05-28-p3-residuals.md::P3-1`

## How to read individual docs

- Each doc starts with a STATUS line (RESOLVED / PARTIAL / OPEN)
- Findings include the canonical Haskell source quote (via cardano-haskell-oracle or cardano-ledger-oracle)
- Code references use `crate/path/file.rs:line` form
- Resolving commits are listed inline
