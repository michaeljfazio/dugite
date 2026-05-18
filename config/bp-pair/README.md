# config/bp-pair/

Configuration for the **preview Sandstone soak rig** — a dugite-node block
producer paired with either a Haskell relay (default) or a dugite relay,
running against the public preview testnet. This is the long-running rig that
forges blocks for the Sandstone pool
(`6954ec11cf7097a693721104139b96c54e7f3e2a8f9e7577630f7856`).

## Files

| File | Used by |
|------|---------|
| `dugite-bp.config.json` | dugite-node block-producer settings (BP-tuned peer targets, log levels). |
| `dugite-bp.topology.json` | Bootstrap peers + on-chain ledger peer discovery for a bare BP. |
| `dugite-relay.config.json` | dugite-node relay settings used when pairing two dugite nodes. |
| `dugite-relay.topology.json` | Relay topology (public roots + churn-bound targets). |
| `haskell-relay.config.json` | cardano-node relay settings used when pairing dugite-BP with a Haskell relay. |
| `haskell-relay.topology.json` | Haskell relay topology pointing at the dugite BP on localhost. |

All four `*.config.json` files reference preview genesis files via the
relative path `../preview/{era}-genesis.json`, so the rig stays consistent
with the canonical preview network configuration.

## How to launch

See `scripts/soak/run-bp-pair.sh` (Haskell relay + dugite BP, default for
`just soak-6h`) and `scripts/soak/launch-bare-bp.sh` (bare BP, default for
`just soak-bare-bp`).
