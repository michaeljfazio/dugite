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
   feeds ep181 WithdrawalAmountMismatch). state:NEW attempts:0
2. [H][ledger] #11 mainnet stake-dereg residual (4 no-withdrawal cases diverge).
   state:NEW attempts:0
3. [H][ledger] mainnet ep213 reward divergence (REWARD-DIVERGENCE-MAINNET-ep213.md).
   state:NEW attempts:0
4. [M][phase2] #22 CEK V1/V2 Babbage residual (budget/Error/unIData buckets).
   state:NEW attempts:0
5. [L][phase2] #14 V3 TxInfo deferred fields (inert until mainnet ep507).
   state:NEW attempts:0

## In-progress
(none)

## Running jobs
(none)

## DB clones on disk
(none)

## Gauntlet ledger  (passed/refuted approaches — never silently retry a REFUTED)
(none)

## Token spend  (rolling; UTC-dated lines)
(none)

## Last node state
- sampled: never
