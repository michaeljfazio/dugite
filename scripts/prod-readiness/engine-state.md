# Engine State  (single source of truth — committed every wake)

## Control
- HALT: false
- refuter_N: 3
- daily_token_budget: 40000000
- cadence_floor_secs: 270
- cadence_ceiling_secs: 1800
- reference_node_socket: none        # Koios-first; set if a cn node is up

## Frontiers  (advance these; zero open divergence behind each)
- ledger.preprod:   epoch 56  (first open divergence at ep57 stake-dist)
- ledger.mainnet:   epoch 212 (open: ep213 reward divergence)
- sync.preprod:     halts at ep181 (WithdrawalAmountMismatch, downstream of ep57)
- sync.mainnet:     ~ep331 (last known good db-mainnet)
- phase2.preprod:   open buckets: budget ~398, Error ~186, unIData ~44 (Babbage V1/V2)
- phase2.mainnet:   inert until ep507 (V3)
- perf:             at-tip CPU bounded (15 hot peers); sync ~300 blk/s Byron

## Backlog  (ranked by impact; one advanced per wake)
1. [H][ledger] ep57 preprod stake-distribution -10 ADA  (2 delegators each -5 ADA;
   root-caused to UTxO-set content / addr->cred attribution, NOT incremental upkeep;
   feeds ep181 WithdrawalAmountMismatch). state:REPRODUCING attempts:1
   blocked-on: acquiring db-preprod-sync (mithril) + epoch-state-debug binary
2. [H][ledger] #11 mainnet stake-dereg residual (4 no-withdrawal cases diverge).
   state:NEW attempts:0  (replayable from db-mainnet; verify its epoch first)
3. [H][ledger] mainnet ep213 reward divergence (REWARD-DIVERGENCE-MAINNET-ep213.md).
   state:NEW attempts:0
4. [M][phase2] #22 CEK V1/V2 Babbage residual (budget/Error/unIData buckets).
   state:NEW attempts:0
5. [L][phase2] #14 V3 TxInfo deferred fields (inert until mainnet ep507).
   state:NEW attempts:0

## In-progress
- item: #1 ep57 preprod stake-distribution -10 ADA
- state: ANALYZING (diagnose muscle Workflow w2ci9weas running — localizing vs Koios)
- attempts: 1
- reproduced: YES. db-preprod-sync (mithril ep293) cloned -> preprod-ep57, from-genesis
  replay produced epoch-state dumps ep0..ep62 at epoch-dumps-engine/preprod-ep57/.
  ep56/57/58 present with stake_snapshot/pools/rewards. Replay SIGTERMed at ep63.
- next: when diagnose Workflow returns the localized divergence, run analyze (research
  Haskell source + spec) -> fix (worktree, Tier A) -> VERIFYING replay -> gauntlet.

## Running jobs
- diagnose-muscle  workflow=w2ci9weas  (Diagnose phase, 3 parallel agents vs Koios)
  (build-epoch-debug + mithril-preprod completed in wake1/early-wake2; binary built,
   db-preprod-sync ready at 17G ep293)

## DB clones on disk
- db-clones/preprod-ep57  (CoW clone of db-preprod-sync; snapshot/haskell-ledger wiped
  for from-genesis replay; reusable for re-verification)

## Gauntlet ledger  (passed/refuted approaches — never silently retry a REFUTED)
(none)

## Token spend  (rolling; UTC-dated lines)
- 2026-06-06T11:41Z wake1 ~ build+mithril launch (assess+drive)
- 2026-06-06T11:52Z wake2 ~ replay reproduce + 2 launch-replay fixes + diagnose Workflow

## Last node state
- sampled: 2026-06-06T11:40Z  node_pids="" rss_mb=0 free_disk_gb=205 free_ram_gb=5 jobs=0 halt=false

## Wake log
- wake1 2026-06-06T11:41Z: ASSESS found no db-preprod-sync (only db-mainnet + db-preprod-haskell
  [cardano-node format]). SCHEDULE picked #1 (top impact); its precondition is a preprod dugite
  db + the epoch-state-debug binary. DRIVE launched both as background jobs under the heavy-op
  lock (build=CPU, mithril=IO — recorded concurrency: different resource classes, both blocking
  prerequisites). RESCHEDULE ~1200s to let both finish.
- wake2 2026-06-06T11:52Z (early; prereqs finished fast): REPRODUCE. Found+fixed 2 launch-replay
  bugs (wipe must remove ledger-snapshot.bin AND haskell-ledger/ import dir, else node loads
  ep293 state and skips from-genesis replay). Clean from-genesis replay produced dumps ep0..62;
  ep56/57/58 captured. Fired diagnose muscle Workflow w2ci9weas (/workflows-visible) to localize
  the ep57 divergence vs Koios. Also committed observability change (diagnose mode routes all
  analytical work through the muscle for /workflows visibility).
