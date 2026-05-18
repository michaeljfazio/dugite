# Development

Day-to-day tasks (build, test, lint, run a network, soak, monitor) are wrapped by a top-level [`justfile`](https://github.com/michaeljfazio/dugite/blob/main/justfile). Install [just](https://github.com/casey/just) (`brew install just`, `cargo install just`, or your package manager) and `just --list` shows the full menu.

## Quick reference

```bash
# Full CI gate (run this before opening a PR).
just check

# Individual gates
just build                  # cargo build --release --all-targets
just test                   # cargo nextest run --workspace
just test-doc               # cargo test --doc
just clippy                 # cargo clippy --all-targets -- -D warnings
just fmt-check              # cargo fmt --all -- --check
just fmt                    # apply rustfmt

# Run a node
just run-relay preview
just run-bp mainnet
just mithril-import preview

# Local devnet (loopback testnet)
just devnet-setup           # one-time
just devnet-run             # start dugite-bp + dugite-relay + cardano-node-bp
just devnet-soak            # 30-minute soak
just devnet-verify          # check evidence
just devnet-stop

# Preview Sandstone soak
just soak-6h                # Haskell relay + dugite BP, 6h orchestrator
just soak-bare-bp           # dugite BP alone
just soak-status

# Monitoring (Prometheus + Grafana via Docker)
just monitor-start
just monitor-status
just monitor-stop
just watch-metrics          # tail the Prometheus endpoint

# Validation
just compat-n2c             # cardano-cli vs dugite-cli N2C diff
just compat-leader          # leader-schedule N2C diff
just submit-txs n=100       # submit N test transactions
just stress-test
just stress-relay
just benchmark-pipeline

# Dev / release helpers
just licenses               # regenerate docs/src/reference/third-party-licenses.md
just clean-worktrees        # prune stale git worktree branches
just query-tip              # dugite-cli query tip against ./node.sock
```

## Layout

The recipes wrap scripts under `scripts/<group>/`:

| Group | Path |
|-------|------|
| Run | `scripts/run/{bp,relay}-{mainnet,preview,preprod}.sh`, `scripts/run/dual-node.sh`, `scripts/run/haskell-relay-preview.sh` |
| Soak | `scripts/soak/orchestrator-6h.sh` + helpers |
| Monitoring | `scripts/monitoring/start.sh`, `watch-metrics.sh`, `health-check.sh`, `bp-watch.sh`, `relay-watchdog.sh` |
| Validation | `scripts/validation/n2c-compat-test.sh`, `leader-schedule-compat.sh`, `stress-test*.sh`, `submit-txs.sh`, `benchmark-pipeline-depth.sh` |
| Mithril | `scripts/mithril/import.sh` |
| Dev | `scripts/dev/check.sh`, `generate-licenses.py`, `cleanup-worktree-branches.sh`, `query-tip.sh` |

Each script does `cd "$(dirname "$0")/../.."` first, so you can invoke them from anywhere — `just`, the repo root, or by absolute path. See [scripts/README.md](https://github.com/michaeljfazio/dugite/blob/main/scripts/README.md) and [config/README.md](https://github.com/michaeljfazio/dugite/blob/main/config/README.md) for the canonical layout description.

## Hard requirements

- **Zero warnings** — `cargo clippy --all-targets -- -D warnings` (also run by `just clippy` and `just check`)
- **Formatted** — `cargo fmt --all -- --check` (`just fmt-check`)
- **All tests pass** — `cargo nextest run --workspace` and `cargo test --doc` (`just test`, `just test-doc`)
- **CI green** before merging
- **Focused commits** — stage explicit filenames; the pre-commit hook warns if staged paths span more than two crates (set `DUGITE_PRECOMMIT_STRICT=1` to make this fatal)
