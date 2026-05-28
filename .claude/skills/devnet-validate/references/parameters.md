# Parameters — slot, epoch, security

## Committed defaults (`testnet/local-devnet/config/spec/shelley-spec.json`)

```json
{
  "slotLength": 1.0,
  "epochLength": 400,
  "activeSlotsCoeff": 0.5,
  "securityParam": 40,
  "updateQuorum": 2,
  "maxLovelaceSupply": 60000000000000000,
  "networkMagic": 42
}
```

## What these mean

| Field | Value | Derived |
|---|---|---|
| `slotLength` | 1.0 s | Wall-clock pacing |
| `epochLength` | 400 slots | 400 s ≈ 6.67 min per epoch |
| `activeSlotsCoeff` (f) | 0.5 | P(slot has leader) ≈ 0.5 |
| Expected blocks/epoch | — | `epochLength × f` = 200 |
| `securityParam` (k) | 40 | Max rollback window |
| Stability check | — | `3k / f` = 240 ≤ `epochLength` = 400 ✓ |
| RUPD pulser check | — | `4k / f` = 320 ≤ `epochLength` = 400 ✓ |

The Shelley genesis stability invariant is `3k/f ≤ epochLength` — each epoch must contain at least 3k active slots in expectation. Violating it makes epoch boundaries non-deterministic w.r.t. nonce evolution.

The Praos RUPD invariant is `4k/f ≤ epochLength` — the reward-update pulser starts at slot `4k/f` of each epoch and must complete before the epoch ends. If this fails, the pulser never runs and treasury/reserves stay at genesis values forever (both dugite and Haskell behave this way — chain stays valid but rewards never accumulate). `k=40` (vs. the original `k=60`) satisfies both invariants and lets Round 2 actually observe pot movement.

## Sizing a round

- **One epoch boundary**: needs ≥ `epochLength × slotLength + ~30s buffer` = 430s (~7 min).
- **First RUPD application**: the pulser starts at slot 4k/f=320 of epoch N, finishes by slot 400, and the resulting reward update is *applied* at the boundary N+1 → N+2 (i.e. slot 800 with epoch_length=400). Round 2's 15-min soak crosses this.
- **Two epoch boundaries** (for snapshot rotation verification + first RUPD application): ≥ 900s (~15 min). Standard for Round 2.

## Overriding per-run

If a special case needs different params (e.g. forcing two boundaries in one round), edit the spec, then re-run setup:

```bash
cd testnet/local-devnet
# Edit config/spec/shelley-spec.json — DO NOT commit unless intentional
./setup.sh    # regenerates genesis with the new spec
./run.sh
```

The spec is deep-merged onto cardano-cli's defaults via `jq -s '.[0] * .[1]'` in `setup.sh`. Only override the fields you actually need.

## Why these specific numbers (and not bigger/smaller)

- **Smaller `epochLength` (e.g. 200)**: would need k ≤ 33 to satisfy stability, k=30 leaves only 90-slot rollback window — risky for restart tests.
- **Larger `activeSlotsCoeff` (e.g. 1.0)**: would put a leader in every slot, but Praos's leader-election entropy collapses; not representative of real chain behaviour.
- **Smaller `slotLength` (e.g. 0.2s)**: faster wall-clock but cardano-node's slot subprocessing hasn't been validated at sub-second cadence on macOS.

## What gets regenerated

`setup.sh` regenerates:
- `genesis/shelley-genesis.json`, `genesis/byron-genesis.json`, `genesis/conway-genesis.json`, `genesis/alonzo-genesis.json`, `genesis/dijkstra-genesis.json`
- Genesis/delegate/utxo/stake-delegator keys under `keys/`
- Pool keys (2 pools: pool1=95% stake, pool2=5%, but only pool1 ever forges because cardano-bp has no operational cert)
- Two seated CC members (`cc-1`, `cc-2`) so `tx-zoo/05-governance-certs` and `07-voting` have live targets
- Per-node config + topology JSONs under `config/`

`setup.sh` is idempotent (rm -rf the prior state). Always re-run it between test rounds.

## Genesis freshness window

`run.sh` checks `systemStart` against wall clock and refuses to start if `|skew| > 300s`. This is intentional: a Cardano node will run a stale genesis but it will spend its boot replaying virtual slots, polluting timing measurements. Always re-`setup.sh` before each round.
