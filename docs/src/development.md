# Development

Day-to-day tasks (build, test, lint, run a network, soak, monitor) are wrapped by a top-level [`justfile`](https://github.com/michaeljfazio/dugite/blob/main/justfile). Install [just](https://github.com/casey/just) (`brew install just`, `cargo install just`, or your package manager); bare `just` (or `just --list`) shows the full menu.

Before your first build, install `protoc` — `dugite-rpc`'s build script needs it. See [System Dependencies](./installation.md#system-dependencies).

Recipe arguments are **positional**, not named: `just submit-txs 100`, not `just submit-txs n=100` (the latter passes the literal string `n=100`).

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

# Local devnet (testnet/local-devnet, loopback)
just devnet-setup           # one-time: render configs, generate keys, fetch reference binaries
just devnet-run             # start dugite-bp + dugite-relay + cardano-node-bp
just devnet-soak            # 30-minute soak
just devnet-verify          # check evidence from the last run/soak
just devnet-report          # single-round report from the latest evidence dir
just devnet-stop

# devnet-validate presets (see the devnet-validate skill)
just devnet-validate-smoke      # single boot, ~5 min — PR gate for core crates
just devnet-validate-extended   # 3 rounds, ~75 min — used for release tagging

# Preview Sandstone soak
just soak-6h                # Haskell relay + dugite BP, 6h orchestrator
just soak-bare-bp           # dugite BP alone
just soak-status

# Monitoring (Prometheus + Grafana via Docker)
just monitor-start          # optional arg: Prometheus port (default 9090)
just monitor-status
just monitor-stop
just watch-metrics          # tail the Prometheus endpoint (default port 12796)

# Validation
just compat-n2c             # cardano-cli vs dugite-cli N2C diff
just compat-leader          # leader-schedule N2C diff
just submit-txs 100         # submit N test transactions (positional arg)
just stress-test
just stress-relay
just benchmark-pipeline

# Dual-decode validation (in-house decoder vs shadow decoder)
just dual-decode-smoke                  # serialization tests with DUGITE_DUAL_DECODE=panic
just dual-decode-soak preview 0         # NETWORK, MAX_BLOCKS (0 = unlimited), extra flags
just dual-decode-report                 # summarise mismatch artefacts

# Upstream conformance (corpus pinned in tests/conformance/upstream/manifest.toml)
just download-upstream-fixtures         # all seven fixture areas at the pinned tag
just download-upstream-fixtures-area plutus
just test-conformance                   # UPLC + every upstream golden test
just test-conformance-uplc              # 999 plutus-core evaluation vectors
just test-conformance-upstream          # all upstream goldens in one binary
just regenerate-corpus-local            # rebuild corpus tarballs locally

# Dev / release helpers
just licenses               # regenerate docs/src/reference/third-party-licenses.md
just clean-worktrees        # prune stale git worktree branches
just query-tip              # dugite-cli query tip against ./node.sock
just bump-utxorpc-spec v0.19.2   # refresh vendored utxorpc/spec protos
```

Per-area conformance filters also exist (`just test-conformance-cardano-base`, `-cardano-ledger`, `-cardano-node`, `-ledger-rules`, `-mithril`, `-ouroboros-consensus`, `-status`). These are for iteration only — the "N skipped" count they print is the tests belonging to *other* areas, not a coverage gap. Use `just test-conformance-upstream` for the unfiltered run.

## Layout

Most recipes wrap scripts under `scripts/<group>/`; the devnet recipes wrap `testnet/local-devnet/`:

| Group | Path |
|-------|------|
| Run | `scripts/run/{bp,relay}-{mainnet,preview,preprod}.sh`, `scripts/run/dual-node.sh`, `scripts/run/haskell-relay-preview.sh` |
| Soak | `scripts/soak/run-6h.sh` (entry point; backgrounds `orchestrator-6h.sh`), `run-bare-bp.sh`, `status-6h.sh` + helpers |
| Local devnet | `testnet/local-devnet/{setup,run,soak,verify,stop,submit-txs,run-genesis}.sh` |
| Monitoring | `scripts/monitoring/start.sh`, `watch-metrics.sh`, `health-check.sh`, `bp-watch.sh`, `relay-watchdog.sh` |
| Validation | `scripts/validation/n2c-compat-test.sh`, `leader-schedule-compat.sh`, `stress-test.sh`, `stress-test-5k.sh`, `relay-stress-test.sh`, `submit-txs.sh`, `benchmark-pipeline-depth.sh`, `dual-decode-soak.sh`, `dual-decode-report.py` |
| Mithril | `scripts/mithril/import.sh` |
| Conformance | `scripts/regenerate-conformance-corpus/regenerate.sh` + per-area `capture-*.sh` |
| Dev | `scripts/dev/check.sh`, `generate-licenses.py`, `cleanup-worktree-branches.sh`, `query-tip.sh`, `bump-utxorpc-spec.sh` |

Most scripts `cd "$(dirname "$0")/../.."` before doing work, so they resolve `./config/` and `./target/release/` regardless of cwd — you can invoke them via `just`, from the repo root, or by absolute path. A minority (notably some under `scripts/monitoring/` and `scripts/validation/`) assume the repo root; prefer the `just` recipe. See [scripts/README.md](https://github.com/michaeljfazio/dugite/blob/main/scripts/README.md) and [config/README.md](https://github.com/michaeljfazio/dugite/blob/main/config/README.md) for the canonical layout description.

## Hard requirements

- **Zero warnings** — `cargo clippy --all-targets -- -D warnings` (also run by `just clippy` and `just check`)
- **Formatted** — `cargo fmt --all -- --check` (`just fmt-check`)
- **All tests pass** — `cargo nextest run --workspace` and `cargo test --doc` (`just test`, `just test-doc`)
- **CI green** before merging
- **Focused commits** — stage explicit filenames; the pre-commit hook warns if staged paths span more than two crates (set `DUGITE_PRECOMMIT_STRICT=1` to make this fatal)
