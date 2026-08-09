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
