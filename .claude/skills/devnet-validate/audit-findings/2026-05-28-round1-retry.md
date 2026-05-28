# Round 1 retry — 2026-05-28 (HEAD: 60d3b0026)

## P0 Plutus past-horizon: RESOLVED

Re-ran the full devnet after the Plutus fix landed (commits `3b9646a32`
+ `28f401c25`).

Evidence:

| Predicate | Round-1 attempt 1 | Round-1 attempt 2 (post-fix) |
|---|---|---|
| tx-zoo total | 83 (80 PASS / 0 FAIL / 3 SKIP) | 83 (80 PASS / 0 FAIL / 3 SKIP) |
| 03-plutus rows | 12/12 PASS | 12/12 PASS |
| `TimeTranslationPastHorizon` rejections by cardano-bp | **1 (block `046d35638b...@slot 267`)** | **0** ✓ |
| `dugite_horizon_rejects` (probe metric) | n/a | 0 across 8 probes (no near-horizon TTLs hit) |
| Haskell-tip parity at each probe (slot gap) | drifted to 49 → 111 → 171 (permanent fork) | ≤1 across 8 probes |
| Epoch boundary 0→1 | not reached before fork | crossed cleanly under tx-zoo load |

Conclusion: the P0 fix is byte-exact Haskell-faithful and the original
TTL=865/horizon=800 class is eliminated.

## NEW finding — P0/P1 — `WithdrawalsNotInRewardsCERTS`

Surfaced by `bidirectional-parity.sh` (which is itself a new diagnostic
from this session). Block `6a04219226ba757624c77fc9bb7653542274d8c4e008578e8e5dbbf21531da66`
forged by dugite-bp at slot 598 contained tx
`e4118c034c7c01bc604dfa3b0c34ba4e624beeab8901cad8fa0d3a5139d04e9c` from
`tx-zoo/04-stake/04g-reward-withdrawal.sh`. Haskell rejected with:

```
ConwayCertsFailure (WithdrawalsNotInRewardsCERTS
  (Withdrawals {unWithdrawals = fromList [
    (AccountAddress {aaNetworkId = Testnet,
                     aaId = AccountId {unAccountId = KeyHashObj
                       (KeyHash {unKeyHash =
                         "ade63a91780621d1deafa0a52dc674d5ecce699bba2d170a31cb4a16"})}},
     Coin 200000000000)
  ]}))
```

dugite-relay (and dugite-bp) admitted the 200_000_000_000 lovelace
withdrawal; Haskell knows the reward account `ade63a917806...` has no
such balance.

### Hypothesis

Three candidate root causes (ranked):

1. **Mempool reward-balance check missing.** dugite's
   `validate_transaction_with_context` does not verify that each
   declared withdrawal `(account, amount)` matches the current
   `rewardsAccount` balance in the ledger state. Haskell
   `ConwayCERTS` runs `WithdrawalsCERTS` rule which checks
   `withdrawal.amount == rewardsBalance(stakeCred)` exactly (no
   over/under, both directions).
2. **Reward-balance divergence**, where dugite's per-stake-key reward
   accumulation diverges from Haskell's RUPD at the boundary 0→1 (one
   epoch crossed during this run). The Sandstone pool's stake
   delegators get rewards in epoch 1 from epoch-0 stake snapshot; if
   dugite's reward distribution rounding / share split differs by even
   one lovelace per delegator, the per-account balance is wrong.
3. **Stake-key activation timing**. dugite's stake key may be activated
   one epoch earlier than Haskell's, distributing it rewards Haskell
   doesn't credit.

Both #1 and #2 are #438/#481-class bugs (memory:
`project_issue_438_*`, `project_issue_481_*`). #1 is the most direct
fix — Haskell mempool admission rejects this exact tx, so dugite
mempool admission should too.

### Reproduce

```bash
cd testnet/local-devnet
./setup.sh && ./run.sh && sleep 30 && ./tx-zoo/run-all.sh --setup
./tx-zoo/run-all.sh                  # batch 1 via relay — 04g PASSes
# Wait for epoch 0→1 (≈400s); reward distribution happens at boundary.
sleep 360
./tx-zoo/04-stake/04g-reward-withdrawal.sh   # may PASS at this point
grep -c 'WithdrawalsNotInRewardsCERTS' logs/cardano-bp.log   # → 1
```

Forensics saved to `/tmp/{cbp,dbp}-r1-attempt2.log`, parity-matrix and
results CSVs in `/tmp/`.

## Bidirectional-parity script — test-design caveat

The wrapper runs the SAME tx-zoo scripts twice (batch 1 via
`LD_RELAY_SOCK`, batch 2 via `LD_CARDANO_BP_SOCK`). 20/41 reported
"OFF-DIAGONAL" cells in `parity-matrix.csv`.

Examination of the failures:

| Category | OFF-DIAG rows | Real bug? |
|---|---|---|
| 01-bookkeeping (01b..01h) | 7 | NO — first batch spent the largest UTxO; second batch has nothing to pay with |
| 04-stake (04b/d/e/f/g) | 5 | NO — stake key already registered / pool already exists from batch 1 |
| 06-proposals (06a-g) | 7 | NO — proposal IDs are derived from tx-in; batch 1's tx-ins already spent |
| 08-negative (08d) | 1 | NO — collateral pool depleted |

The "off-diagonal" pattern is **chain-state mutation between batches**,
not accept-set asymmetry. The wrapper as written cannot enforce the
bidirectional parity oracle as described in
`references/test-methodology.md`. To make it real we need either:

1. Submit a SINGLE physical signed tx CBOR to both sockets and
   compare each socket's accept/reject outcome (this is the
   "same tx" oracle Haskell tests use). OR
2. Run each script twice but with independent funded keys so the
   second batch has fresh state. OR
3. Define a CURATED subset of scripts that ARE idempotent on the
   chain (negatives in `08-*` mostly are — they never spend a UTxO
   on success).

Recommend option (3) for the next iteration:

```bash
# Only negative-classification scripts are safely re-runnable across
# sockets — they all reject pre-mempool without state mutation.
../../.claude/skills/devnet-validate/scripts/bidirectional-parity.sh 08-negative
```

## Skill diagnostics: WORKING

The two new skill additions this session both proved their worth:

- `health-probe.sh` with socket-fallback (commit `e73cb5d26`) caught
  the new invalid block within 60s of it occurring.
- `bidirectional-parity.sh` (commit `70d366ea0`) revealed the
  `WithdrawalsNotInRewardsCERTS` divergence by submitting 04g via the
  relay path, getting PASS, and then noticing Haskell rejected the
  block.
- The auto-restart-detection added in commit `60d3b0026` eliminated
  the false-SICK PROBE 1 events.

## Status

- Round 1 baseline: **NOT YET PASSING**. Fails on
  `WithdrawalsNotInRewardsCERTS` (new finding) and on the
  bidirectional-parity test-design issue.
- Decision point — see `dugite_node_validate_round1_status` in the
  conversation.
