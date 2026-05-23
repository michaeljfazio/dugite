# Haskell Cardano Ledger — Reference

Comprehensive era-by-era ground-truth reference compiled by dispatching
`cardano-haskell-oracle` agents to deeply read the IntersectMBO/cardano-ledger
source code. Used by future sessions to skip live-research and get answers
fast.

Each file is a structured Markdown document with:
- Rules, sub-rules, and exact ordering
- State mutations, pre-conditions, and predicate failures
- Verbatim quotes from the Haskell source with GitHub permalinks

## Files

| File | Coverage |
|---|---|
| [shelley-core.md](shelley-core.md) | Shelley LEDGER, EPOCH, NEWEPOCH, TICK |
| [shelley-rewards.md](shelley-rewards.md) | RUPD, SNAP, PulsingReward, mkPoolRewardInfo, applyRUpd, snapshot lifecycle |
| [shelley-certs.md](shelley-certs.md) | DELEG, POOL, PPUP, NEWPP, POOLREAP, MIR + TICK nonce evolution |
| [alonzo.md](alonzo.md) | Plutus, IsValid, collateral, ExUnits, cost models |
| [babbage.md](babbage.md) | Reference inputs, inline datums, ref scripts, collateral_return |
| [conway.md](conway.md) | Governance (DReps, committees, gov actions), RATIFY/ENACT |
| [dijkstra.md](dijkstra.md) | Sub-txs, direct deposits, account_balance_intervals, PV4 |

## Notes

These docs are written for a Rust reimplementation that needs byte-exact
parity with cardano-node. Be precise about ordering, formulas, and exact
field names. When in doubt, defer to the Haskell source quoted inline.
