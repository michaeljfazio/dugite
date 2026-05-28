# Audit findings index

Status summary of every findings document. **Read this first** to see
what's resolved, what's in-progress, and the resolving commit for each.

| Document | Status | Resolving commit(s) | Notes |
|---|---|---|---|
| [`2026-05-28-skill-self-audit.md`](./2026-05-28-skill-self-audit.md) | **RESOLVED** | A1-A4 (`bd9b8e6` etc); A8 PeerCooling (`89170444f`, `46a5261c6`) | Two A8 divergences (peer-counter overlap, hot/warm/cold dedup) were false-positives per cardano-haskell-oracle re-verification — dugite was already Haskell-faithful |
| [`2026-05-28-round1-retry.md`](./2026-05-28-round1-retry.md) | **RESOLVED** | `0821af5d2` (last Round 1 attempt PASS) | 4 P0 mempool-admission gaps fixed: TimeTranslationPastHorizon, WithdrawalsNotInRewardsCERTS, MissingVKeyWitness for voters, MissingVKeyWitness for collateral |
| [`2026-05-28-round2-rupd-divergence.md`](./2026-05-28-round2-rupd-divergence.md) | **RESOLVED** | `037c464ea` | Conway-from-genesis RUPD: `prev_d=1/1` default + snapshot pre-fill caused 3.6T divergence at boundary 0→1 and 22 ADA at 1→2. Fix scoped to Conway-from-genesis init |
| [`2026-05-28-conway-ledger-predicate-audit.md`](./2026-05-28-conway-ledger-predicate-audit.md) | **RESOLVED** | `9222c9387` (DRep.4) + `9ee6164d9` (Babbage.2/3) + `19c86570e` (POOL.3 + WrongNetworkPOOL + GOV.13/17 + Shelley.8) | All 42 originally-🔍 entries closed: 24 → ✅ (already implemented), 13 → ✅ (newly implemented), 6 deprecated/non-Conway → ✅ (verified N/A). 0 ❌ remaining |
| [`2026-05-28-p3-residuals.md`](./2026-05-28-p3-residuals.md) | **RESOLVED** | `verify.sh` SKIP track (`b4365c96d`); multi-BP attribution `LD_P2_SMALL_STAKE_POOL2` env var (`05d5d8a98`); 22.14B reserves residual closed by `0d79e7075` (mark-only-prefill at Conway-from-genesis init) | All items resolved; see individual doc for details |
| [`2026-05-28-session-summary.md`](./2026-05-28-session-summary.md) | **META** | `6bd42da17` | High-level session summary; lists all 6 P0 + 1 P1 + N P2 fixes |

## What's truly open after this session

**Nothing.** All audit findings, P0/P1/P2/P3 follow-ups, and the 22.14B reserves residual are resolved as of `0d79e7075`. All 3 measurable boundaries (0→1, 1→2, 2→3) on the Conway-from-genesis devnet are now byte-exact with Haskell on both treasury and reserves.

The final closure was a `mark`-only pre-fill at Conway-from-genesis init — verified by capturing a live `cardano-cli ledger-state` JSON at boundary 2→3 of a running devnet, identifying that Haskell distributes 22.14B lovelace across 20 stake credentials (1.107B each), and adjusting dugite's SNAP timing to match.

## How to read individual docs

- Each doc starts with a STATUS line (RESOLVED / PARTIAL / OPEN)
- Findings include the canonical Haskell source quote (via cardano-haskell-oracle or cardano-ledger-oracle)
- Code references use `crate/path/file.rs:line` form
- Resolving commits are listed inline
