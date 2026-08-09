# Mainnet treasury divergence — dugite accumulates from ~0 where mainnet starts at 1.58e15

Status: **OPEN — P0 candidate. Evidence gathered, cause NOT yet established.**
Found: 2026-08-09, by replaying mainnet from genesis to epoch 267.

## The measurement

A dugite node replayed mainnet Byron→Mary from genesis: **5.74 M blocks, epoch
267, 0 ERROR/panic**, clean through the Shelley→Allegra AVVM boundary.

| | treasury (lovelace) |
|---|---|
| dugite, epoch 267 (`dugite_treasury_lovelace`) | 448,318,686,397,699 |
| Koios, epoch 267 | 1,981,939,006,583,972 |

Koios treasury by epoch: **208 → 1,580,272,859,467,751**, 210 → 1,593,078,596,497,752,
218 → 1,647,357,233,912,817, 236 → 1,768,354,601,316,244, 267 → 1,981,939,006,583,972.

## Why this looks like a MISSING INITIAL VALUE, not missing accumulation

Mainnet gains **+389 T** from epoch 210 to 267. dugite reports **448 T** total.
Those are the same order — consistent with dugite accumulating the per-epoch
`deltaT1` correctly while never having the initial ~1.58e15 that mainnet
already carried at epoch 208.

`eras/shelley.rs::on_era_transition` states the assumption outright:

> "Recompute from the live UTxO so reserves matches Haskell exactly
> (**treasury, rewards and deposits are all 0 at the fork**)."

That is the claim Koios contradicts.

## Ruled OUT

- **Not #1072's gate.** The gate declined exactly ONCE, at epoch 208 (the
  Byron→Shelley transition, where it is expected and where
  `is_byron_to_shelley_fork` guards it anyway). Every later epoch applied a
  reward update. Checked before assuming — the tempting fix here would have
  been to weaken a correct consensus gate.
- **Not a mid-epoch sampling artefact.** The gap is ~4.4x; one epoch's tau cut
  at epoch-208 reserves is `0.2 * 0.003 * 13.4e15 ≈ 8.0e12`, three orders of
  magnitude too small to explain 1.58e15.
- **Not the AVVM return.** That lands at Shelley→Allegra (236) and the shortfall
  predates it — dugite is already low relative to epoch 208.

## NARROWED 2026-08-09 — it is the UTxO translation, NOT a missing treasury seed

Upstream is unambiguous (`translateToShelleyLedgerStateFromUtxo`):

```haskell
casTreasury = Coin 0
casReserves = word64ToCoin (fbtcMaxLovelaceSupply transCtxt) <-> sumCoinUTxO utxoShelley
```

Treasury IS zero at the fork, so dugite's stated assumption matches the code and
"dugite forgot to seed the treasury" — my own leading hypothesis — is REFUTED.

Comparing BOTH pots reframes it entirely:

| | treasury | reserves | sum |
|---|---:|---:|---:|
| dugite @267 | 448,318,686,397,699 | 12,341,518,536,205,146 | 12,789,837,222,602,845 |
| Koios @267 | 1,981,939,006,583,972 | 13,000,024,015,340,562 | 14,981,963,021,924,534 |
| delta | **-1,533,620,320,186,273** | **-658,505,479,135,416** | **-2,192,125,799,321,689** |

**Reserves are LOW too.** That also kills the compounding story: if the missing
treasury were still sitting in reserves, reserves would be HIGH. dugite's entire
non-circulating supply is **2.19e15 short**.

Since `reserves = maxLovelaceSupply - sumCoinUTxO(utxoShelley)` at the fork, a
UTxO sum ~2.19e15 TOO LARGE produces exactly this — dugite counts lovelace into
the Shelley UTxO that upstream excludes.

**So the defect is in what dugite carries across the Byron->Shelley UTxO
translation, not in the pot arithmetic.** The per-epoch accumulation is
consistent with the reserves dugite has, so the inputs are wrong and the formula
is right. That also explains the residual: dugite's ~7.6e12/epoch against
mainnet's ~6.8e12 is `rho * reserves` on a different base.

**Next**: sum dugite's UTxO at the fork and diff against
`maxLovelaceSupply - reserves_mainnet@208`. Prime candidates are the AVVM/redeem
UTxOs (upstream keeps them until Shelley->Allegra) and Byron genesis
`nonAvvmBalances`.

## Superseded readings (kept deliberately — both were plausible, both wrong)


1. **dugite does not seed the treasury at the Byron→Shelley translation.**
   `translateToShelleyLedgerStateFromUtxo` would then set a non-zero treasury
   that dugite hardcodes to 0. This is the reading the arithmetic supports.
2. **Koios's `totals.treasury` for epoch N is not the value dugite's gauge
   reports at epoch N** — a definitional mismatch rather than a ledger defect.

Reading 2 must be excluded before any code changes. **Do not "fix" this by
adding a constant that makes the number match** — that is how #1057 was made
worse.

## Next steps, in order

1. Read `Cardano.Ledger.Shelley.API.ByronTranslation.translateToShelleyLedgerStateFromUtxo`
   and establish what it sets `_treasury` to. This is the decisive fact.
2. Cross-check one epoch against a cardano-node `debug log-epoch-state` dump
   rather than Koios — the standing rule is that a cardano-node dump beats
   Koios, which is sanity-only.
3. Only then change code, and re-replay to epoch 236+ to confirm.

## Why no testnet caught this

preview, preprod and the devnet all genesis **post-Byron**, so none has a
Byron→Shelley translation and all start with treasury 0 — where dugite's
assumption is correct. Mainnet is the only network that exercises it, which is
exactly why the replay was run.


---

## MEASURED AT THE FORK 2026-08-09 — dugite's own instrumentation

`eras/shelley.rs` logs the translation, so the inputs are directly observable
rather than inferred:

```text
Byron->Shelley: reserves = maxLovelaceSupply - sumCoinUTxO(liveByronUTxO)
  utxo_sum        = 31,111,977,147,073,356
  new_reserves    = 13,888,022,852,926,644     (= 45e15 - utxo_sum, self-consistent)
  redeem_sum      =      318,200,635,000,000   (465 AVVM redeem UTxOs)
  nonredeem_sum   = 30,793,776,512,073,356     (729,474 UTxOs)

Shelley->Allegra: returnRedeemAddrsToReserves
  redeem_coin     =      318,200,635,000,000   credited to reserves
  reserves_after  = 13,131,756,221,890,729
```

Against Koios epoch 208 (`reserves 13,406,339,661,812,158`):

| | value |
|---|---:|
| dugite reserves at fork | 13,888,022,852,926,644 |
| expected (Koios @208) | 13,406,339,661,812,158 |
| **delta** | **+481,683,191,114,486** (dugite HIGH) |
| of which AVVM redeem | 318,200,635,000,000 |
| **unexplained** | **+163,482,556,114,486** |

**THE SIGN FLIPS.** At the fork dugite's reserves are HIGH by 481.7e12 (so its
UTxO is LOW). By epoch 267 its non-circulating supply is LOW by 2.19e15 (so its
UTxO is HIGH). Two distinct effects, not one:

1. **At the fork** — dugite's Byron UTxO sum is ~481.7e12 too LOW. The AVVM
   redeem set (318.2e12) is the large part and is *deliberately* still in the
   UTxO at this point per upstream, so the comparison against a post-AVVM Koios
   figure double-counts it; the genuinely unexplained remainder is ~163.5e12.
2. **Across Shelley+** — something adds ~2.19e15 to dugite's UTxO relative to
   mainnet, or fails to remove it. This is the larger effect and it is NOT the
   fork translation.

So the fork translation is at most a minor contributor and the dominant defect
is downstream of it. The next measurement must be a per-epoch pot comparison to
find the FIRST epoch where the delta appears — a single number at epoch 267
cannot distinguish a step change from a slow drift, and those have completely
different causes.


---

## STRONGEST LEAD 2026-08-09 — reserves do not move across a boundary

Two samples from the same running node, epochs 267 and 269:

| | treasury | reserves |
|---|---:|---:|
| dugite @267 | 448,318,686,397,699 | 12,341,518,536,205,146 |
| dugite @269 | 463,139,673,075,643 | 12,341,518,536,205,146 |
| **change** | **+14,820,986,677,944** | **0 — UNCHANGED** |
| mainnet change | +13,682,748,182,569 | **-13,839,565,138,166** |

**dugite's treasury grew by 14.8e12 across two boundaries while its reserves
did not move by a single lovelace.** Mainnet's reserves fell by 13.8e12 over
the same span, which is the expected `deltaR1` draw.

Treasury is credited and reserves are not debited. Either:

1. `delta_reserves` is not being applied at `applyRUpd` — a conservation
   defect that CREATES lovelace, which would compound every epoch and is the
   right order of magnitude to explain a 2.19e15 gap accumulated over ~60
   epochs; or
2. `dugite_reserves_lovelace` is a stale gauge and the ledger is fine.

(2) must be excluded first, and cheaply: read reserves from the ledger rather
than the metric at two epochs. If the ledger also shows no movement, it is (1)
and the defect is in the reserves side of `apply_pending_reward_update`.

Note this is measured on a node WITHOUT the pot-parity gate that the devnet and
preprod runs pass — those compare a single instant, and a pot that never moves
matches a pot that never moves whenever both start equal. That is why a
long-running mainnet replay found it and two green gates did not: the devnet
never runs enough boundaries for the drift to separate from noise.


---

## RESOLVED TO A STEP CHANGE 2026-08-09 — and the conservation lead is DEAD

Sampling the same node at three epochs:

| epoch | treasury delta | reserves delta | **sum** |
|---|---:|---:|---:|
| 267 | -1,533,620,320,186,273 | -658,505,479,135,416 | **-2,192,125,799,321,689** |
| 269 | -1,532,482,081,690,898 | -644,665,913,997,250 | **-2,177,147,995,688,148** |
| 270 | -1,531,725,612,537,987 | -661,191,167,040,885 | **-2,192,916,779,578,872** |

**The sum is FLAT at -2.19e15.** The +/-15e12 wobble is mid-epoch sampling, not
drift. So:

* **It is a STEP CHANGE, not a per-epoch formula error.** Per-epoch
  accumulation is CORRECT — dugite debits reserves and credits treasury at the
  right rate. Every "the reward formula is wrong" hypothesis is dead.
* **The conservation lead is DEAD.** At epoch 270 reserves moved
  -23,196,836,280,832. The identical values at 267 and 269 were two samples of
  the SAME ledger epoch, not a pot that never moves. The gauge is live and
  reserves are debited. **I nearly wrote a fix for a defect that does not
  exist** — the "reserves are never debited" reading was wrong, and only a
  third sample showed it.

That is the sixth hypothesis killed by measurement in this investigation
(#1072's gate, the treasury seed, the fork translation as sole cause, the
supply constant, the stale gauge, and now conservation). Each was plausible;
each would have produced a confident wrong change to a consensus path.

## What remains

A single ~2.19e15 step, early in the chain, of which ~1.53e15 sits in treasury
and ~0.66e15 in reserves. dugite's treasury shortfall (-1.53e15) is close to
mainnet's epoch-208 treasury (1.58e15), which upstream's
`translateToShelleyLedgerStateFromUtxo` sets to `Coin 0` — so either Koios's
`totals.treasury` counts something dugite does not model, or mainnet's treasury
was credited by an event between the fork and the end of epoch 208 that dugite
skips.

**The remaining measurement is a per-epoch series across 208-215**, not another
sample at 270. A flat delta at the tail cannot say WHERE the step happened;
only walking the early epochs can. That requires a replay with per-boundary pot
logging, which is the next session's first task.
