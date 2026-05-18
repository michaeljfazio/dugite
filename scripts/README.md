# scripts/

Operational scripts for running, monitoring, and validating Dugite. Most
recipes here are also exposed through the top-level `justfile` — see
`just --list` for the full menu.

## Layout

| Directory | Purpose |
|-----------|---------|
| `run/` | One-shot launchers for a single dugite node — `bp-{mainnet,preview,preprod}.sh`, `relay-{mainnet,preview,preprod}.sh`, plus dual-node and Haskell-relay variants. Used directly or via `just run-bp <network>` / `just run-relay <network>`. |
| `soak/` | Long-running soak rigs for the preview Sandstone pair (6h orchestrator + helpers, bare-BP variant, restart helper). Entry points: `just soak-6h`, `just soak-bare-bp`. |
| `monitoring/` | Prometheus + Grafana stack (`start.sh`), live tailers (`watch-metrics.sh`, `bp-watch.sh`, `relay-watchdog.sh`), and `health-check.sh`. |
| `validation/` | Compatibility suites (`n2c-compat-test.sh`, `leader-schedule-compat.sh`), throughput rigs (`stress-test.sh`, `stress-test-5k.sh`, `relay-stress-test.sh`, `benchmark-pipeline-depth.sh`), and `submit-txs.sh` for one-shot tx submission. |
| `mithril/` | Snapshot-import helpers — `import.sh <network>`. Used to seed `db-<network>/` from a Mithril snapshot. |
| `dev/` | Developer utilities — workspace `check.sh`, license-file regenerator, worktree branch cleanup, `query-tip.sh`. |

## Conventions

- Scripts assume they're invoked from the repo root or via their absolute
  path; each one does `cd "$(dirname "$0")/../.."` before doing work, so they
  resolve `./config/` and `./target/release/` correctly regardless of cwd.
- Scripts that read keys default to `./keys/` (and pool-specific keys under
  `./keys/<network>-test/pool/`). They print a clear error and exit if a
  required key is missing.
- Long-running scripts write logs to `./logs/<rig>/` (gitignored).
- The full list is also discoverable through `just --list` — use that as the
  starting point rather than running scripts directly.

## Where to look first

| Want to… | Use |
|---------|-----|
| Run a node | `just run-bp <network>` or `just run-relay <network>` |
| Bootstrap state | `just mithril-import <network>` |
| Start metrics stack | `just monitor-start` |
| Run the long soak | `just soak-6h` |
| Validate against cardano-cli | `just compat-n2c` / `just compat-leader` |
| Spin up the local testnet | `just devnet-setup && just devnet-run` |
