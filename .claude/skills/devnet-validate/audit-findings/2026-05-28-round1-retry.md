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

## Round 1 attempt 3 — `WithdrawalsNotInRewards` RESOLVED, new bug surfaced

After commit `779922596` (plumb reward_accounts), Round 1 attempt 3:

| Predicate | Result |
|---|---|
| tx-zoo | 80 PASS / 0 FAIL / 3 SKIP |
| 03-plutus rows | 12/12 PASS |
| Health probes 1-9 (during tx-zoo + early bidir-parity) | **all HEALTHY** |
| Haskell-tip parity | within 1 slot throughout the first 9 minutes |
| Epoch boundary 0→1 | crossed cleanly |
| `WithdrawalsNotInRewardsCERTS` recurrences | **0** ✓ |
| `TimeTranslationPastHorizon` recurrences | **0** ✓ |
| 09-cli-parity | PASS (16 EQUAL / 0 DIVERGENT-non-known / 4 ERROR-warning) |
| cross-validate-cli | 7/7 PASS |
| protocols/run.sh (adversarial N2N) | 7/7 PASS |
| bidirectional-parity (08-negative, with per-batch isolation) | **PASS, 0 off-diagonal** ✓ |

## Round 1 attempt 3 — NEW finding `MissingVKeyWitnessesUTXOW`

At PROBE 10 (slot 658), a third class of dugite-vs-Haskell divergence
surfaced. Block
`fb4da1990e8663000e371f170900e98cb2d7cb215d439f45de7c60d16a266762`
forged at slot 645 with 1 tx; Haskell rejected with:

```
ConwayUtxowFailure (MissingVKeyWitnessesUTXOW
  (NonEmptySet (fromList [
    KeyHash {unKeyHash = "b7ec15e8e167637991f151cb3a209171dc722e30f904f5f0310c5043"}
  ])))
```

dugite admitted a tx that was missing a required vkey witness; Haskell
rejected on block-apply. Tip-bifurcation: dugite continued to slot 781;
cardano-bp stuck at slot 643 / block 320.

**Hypothesis**: dugite's Phase-1 already has a `MissingRequiredSigner`
predicate in `validation/mod.rs` and a `MissingVKeyWitnesses` predicate
in `validation/phase1.rs`. The bug is likely one of:

1. The check is gated on a predicate that doesn't cover all the
   cases Haskell does (e.g. only checks `required_signers` field but
   not the implied set of `inputs.addr.payment_key`, certificate
   authorizing keys, withdrawal reward-account keys, etc.).
2. The check sees ADDITIONAL keys provided by witnesses but doesn't
   account for the boot key set from the genesis utxo wallet correctly.

Tx came from the bidirectional-parity setup phase (per-batch funded
key creation around 06:35:50–55Z; slot 645 = 06:35:54Z forge).

**Reproduce**: run `bidirectional-parity.sh 08-negative` with the
current devnet — it submits two genesis-funded sub-account creation
txs, then per-batch tx-zoo --setup which creates many more wallet txs.
One of these is the offender.

## Pattern across attempts 1-3

| Attempt | Bug class | Where dugite went wrong |
|---|---|---|
| 1 | `TimeTranslationPastHorizon` | Plutus context-builder didn't enforce safe-zone horizon |
| 2 | `WithdrawalsNotInRewardsCERTS` | Mempool admission didn't pass `reward_accounts` into ValidationContext |
| 3 | `MissingVKeyWitnessesUTXOW` | Witness-check predicate appears to differ from Haskell's exact required-key set |

All three are the same architectural class: **dugite's mempool admission
is incomplete relative to Haskell's `LEDGER` rule**. Each fix follows
the same pattern — plumb the missing predicate data, mirror the
Haskell rule byte-exact. The bug-cascade suggests there are likely 1-3
more checks dugite is missing on edge cases. Each requires its own
investigation + targeted fix iteration.

## Status

- Round 1 baseline (committed work): **PARTIAL PASS** —
  P0 Plutus past-horizon RESOLVED, P0 WithdrawalsNotInRewards
  RESOLVED, but a third P0 (`MissingVKeyWitnessesUTXOW`) surfaced and
  is unfixed at session end.
- The pattern of newly-discovered dugite-vs-Haskell mempool admission
  gaps is systematic, not one-off. Recommend dedicated audit pass on
  every Haskell `LEDGER` rule predicate vs dugite's
  `validate_transaction_with_context`.
