# config/

Network configurations, monitoring assets, and reusable example fragments
consumed by `dugite-node`, `cardano-node`, the Docker image, and the Helm
chart.

## Layout

| Path | Purpose |
|------|---------|
| `mainnet/` | Cardano mainnet config, topology, and four genesis files. |
| `preview/` | Cardano preview testnet config, topology, and genesis files. |
| `preprod/` | Cardano preprod testnet config, topology, and genesis files. |
| `bp-pair/` | Sandstone preview soak rig — paired dugite-BP + dugite-relay + Haskell-relay configs. References `preview/` genesis via `../preview/`. |
| `monitoring/` | Grafana dashboard JSON, Prometheus scrape + alert rules. Consumed by `scripts/monitoring/start.sh`. |
| `examples/` | Reusable example payloads (e.g. `pool-metadata.json`). |

Each network directory contains a stable `config.json` + `topology.json` pair
plus four era genesis files (`byron-`, `shelley-`, `alonzo-`, `conway-`). The
config files reference the genesis files using **relative paths** so the
network directories are self-contained — move or symlink the whole network
folder and it keeps working.

## Common entry points

- Run a relay on preview: `just run-relay preview` or
  `./scripts/run/relay-preview.sh`
- Run a BP on mainnet: `just run-bp mainnet` or `./scripts/run/bp-mainnet.sh`
- Start monitoring (Prometheus + Grafana, Docker): `just monitor-start`
- 6h soak on preview Sandstone pair: `just soak-6h`

The Docker image bundles `config/` at `/opt/dugite/config/` and defaults to
running with `config/preview/config.json` + `config/preview/topology.json`.
The Helm chart's `configmap-network.yaml` references the per-network genesis
files at `/opt/dugite/config/{network}/{era}-genesis.json`.
