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

## The two open readings

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
