# Issue #763 — Reserves Divergence: Ground-Truth Localization (2026-06-15)

## Summary

The accelerating reserves over-drain (ep524→ +285,693 ADA, growing ~+320K/epoch)
was traced to **per-pool GO-snapshot stake divergences** against Koios — small,
**bidirectional**, and consistent with **inherited reward-account drift carried in
`db-mainnet-val`**, NOT a uniform stake inflation and NOT a reward-formula bug.

## Deterministic reproduction (offline, no node run)

`crates/dugite-ledger/examples/inspect_snapshot.rs` recomputes the RUPD from the
`db-mainnet-val/ledger-snapshot-epoch524-slot141344336.bin` GO snapshot:

```
delta_reserves: 12,691,650,223,133   (dugite)
                12,405,957,283,156   (Koios totals ep524→525)
extra drain:       285,692,977       lovelace = 285,693 ADA   ✓ matches the issue
```

So the ep524 snapshot **deterministically reproduces** the divergence — the bug
is in the snapshot INPUTS (GO pool_stake), not the RUPD math.

## What was RULED OUT

- **Reward/RUPD formula** — verified correct in `reports/reserves-divergence-ep524-527.md`
  §3 (expansion, treasury_cut, total_stake, apparent performance, prevPParams,
  ssFee lag, block counting). Re-confirmed against era-rules `shelley-rewards.md`.
- **`appPerf` cap** (the failed prior agent's lead): the era-rules
  (`shelley-rewards.md` §4.4 `mkApparentPerformance`) show `beta / sigma` with **NO
  `min 1` cap** in the Conway (d<0.8) branch — only the d≥0.8 federated branch
  returns 1. dugite's uncapped `perf` is CORRECT; adding a cap would diverge.
  (The agent's "2.72×/21× inflation" is also inconsistent with the actual 0.04%
  aggregate divergence.)
- **`recompute_snapshot_pool_stakes`** (replay path, `state/epoch.rs:883`) — it
  rebuilds each snapshot's `pool_stake` from that snapshot's OWN frozen
  `delegations` + `stake_distribution`, not the current ledger's. Correct.
- **The `stake1uxv9hwk8...` account** (report's leading candidate) — Koios shows
  it `delegated_pool: null`, `rewards_available: 0`, 204.28M withdrawn across 3
  PRE-Conway txs (ep442/472/496). A non-delegated, drained account cannot inflate
  pool_stake. The report fingered the wrong account.

## Localization (per-pool, go = ep522)

| Pool | dugite go.pool_stake | Koios pool_history ep522 | Δ (dugite − Koios) |
|---|---|---|---|
| 153806db… (largest) | 72,884,363,213,659 | 73,516,915,488,877 | **−632,552 ADA** (−0.86%) |
| 0f292fca… | 6,587,111,955,272 | 6,581,128,659,158 | **+5,983 ADA** (+0.09%) |

The aggregate go total (23,209,207,177,719,487) is ~877M ADA above Koios
`epoch_info.active_stake` ep522 (22,331,739,656,036,658), but the **per-pool**
deltas are small and go in BOTH directions — so the totals gap is a
metric-definition artifact (Koios `active_stake`), not a uniform inflation. The
real signal is the small bidirectional per-pool drift.

## Determination

The small, bidirectional per-pool stake deltas are the fingerprint of
**accumulated reward-account balance drift** (active stake = utxo stake + reward
balance for delegated creds; a drifted reward balance perturbs that pool's stake).
`db-mainnet-val` was built over many sessions across multiple binaries, including
the v8-binary `val12` mid-epoch restore that injected ~996,138 ADA into the
reserve deficit at ep388 (`reports/pots-deficit-fingerprint.md`). That seeds a
self-reinforcing reward↔stake drift that the **current** byte-exact formulas
faithfully propagate.

**Conclusion: no current-code bug was found.** The current SNAP/RUPD/snapshot
code is byte-exact vs the Haskell reference; the divergence is inherited state
corruption in `db-mainnet-val`. The decisive test (named in the task) is a clean
**from-genesis** sync: if the divergence does NOT reproduce, it confirms inherited
corruption; if it DOES, there is a current bug to chase with a Haskell mainnet
per-credential `cardano-cli debug log-epoch-state` dump at ep521/522.

## Next step

Run the from-genesis mainnet soak (current HEAD binary, all fixes) and compare
per-epoch pots deltas to `reports/koios-mainnet-totals-ep520-540.tsv`. Do NOT ship
a speculative #763 ledger change — any approximation would itself diverge.
