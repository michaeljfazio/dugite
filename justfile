# Dugite task runner. Install: https://github.com/casey/just
#
# Recipes are grouped: build & dev, run a network, local devnet, soak,
# monitoring, validation, mithril, dev/release. Run `just` (or `just --list`)
# to see them all.

set shell := ["bash", "-cu"]
set positional-arguments := true

# Show the recipe list.
default:
    @just --list --unsorted

# ─── Build & dev ─────────────────────────────────────────────────────────────

# Release build of every workspace target.
build:
    cargo build --release --all-targets

# Run all workspace tests under nextest (matches CI).
test:
    cargo nextest run --workspace

# Run doc tests (nextest can't, so this stays cargo test).
test-doc:
    cargo test --doc

# Lint with clippy; warnings are errors.
clippy:
    cargo clippy --all-targets -- -D warnings

# Apply rustfmt across the workspace.
fmt:
    cargo fmt --all

# Verify rustfmt is clean (CI gate).
fmt-check:
    cargo fmt --all -- --check

# Full CI gate: fmt-check, clippy, build, test, doc-test.
check: fmt-check clippy build test test-doc

# ─── Run a network ───────────────────────────────────────────────────────────

# Run dugite as a block producer on a given network (mainnet|preview|preprod).
run-bp network:
    ./scripts/run/bp-{{network}}.sh

# Run dugite as a relay on a given network (mainnet|preview|preprod).
run-relay network:
    ./scripts/run/relay-{{network}}.sh

# Import a Mithril snapshot to seed the database. NETWORK ∈ preview|preprod|mainnet.
mithril-import network:
    ./scripts/mithril/import.sh {{network}}

# ─── Local devnet (testnet/local-devnet) ─────────────────────────────────────

# First-time setup: render configs, generate keys, fetch reference binaries.
devnet-setup:
    ./testnet/local-devnet/setup.sh

# Start dugite-bp, dugite-relay, and cardano-node-bp on the loopback testnet.
devnet-run:
    ./testnet/local-devnet/run.sh

# Run the 30-minute local-devnet soak.
devnet-soak:
    ./testnet/local-devnet/soak.sh

# Validate evidence captured by the last devnet run/soak.
devnet-verify:
    ./testnet/local-devnet/verify.sh

# Stop all local-devnet processes.
devnet-stop:
    ./testnet/local-devnet/stop.sh

# ─── Preview Sandstone soak (BP-pair + bare-BP) ──────────────────────────────

# Launch the 6h orchestrated soak (Haskell relay + dugite BP).
soak-6h:
    ./scripts/soak/run-6h.sh

# Launch the 6h orchestrated soak using a bare BP (no local Haskell relay).
soak-bare-bp:
    ./scripts/soak/run-bare-bp.sh

# Show progress of a running 6h soak.
soak-status:
    ./scripts/soak/status-6h.sh

# ─── Monitoring (Prometheus + Grafana) ───────────────────────────────────────

# Start the local Prometheus + Grafana stack (Docker).
monitor-start:
    ./scripts/monitoring/start.sh

# Stop the monitoring stack.
monitor-stop:
    ./scripts/monitoring/start.sh stop

# Show monitoring container status.
monitor-status:
    ./scripts/monitoring/start.sh status

# Tail the dugite Prometheus endpoint (port defaults to 12798).
watch-metrics port="12798":
    ./scripts/monitoring/watch-metrics.sh {{port}}

# ─── Validation rigs ─────────────────────────────────────────────────────────

# N2C compatibility suite: run dugite-cli queries against both nodes and diff.
compat-n2c *args="":
    ./scripts/validation/n2c-compat-test.sh {{args}}

# Leader-schedule N2C compatibility test.
compat-leader *args="":
    ./scripts/validation/leader-schedule-compat.sh {{args}}

# Submit N self-to-self transactions on preview (default 100).
submit-txs n="100":
    N={{n}} ./scripts/validation/submit-txs.sh

# Mempool stress: spam dugite with txs.
stress-test:
    ./scripts/validation/stress-test.sh

# Relay-fetch stress (long-running chain prefetch).
stress-relay:
    ./scripts/validation/relay-stress-test.sh

# Benchmark sweep over pipeline depths.
benchmark-pipeline:
    ./scripts/validation/benchmark-pipeline-depth.sh

# ─── Dual-decode validation (M5 pallas-removal infrastructure) ───────────────

# Local smoke run: serialization tests with DUGITE_DUAL_DECODE=panic (mirrors the PR CI job).
dual-decode-smoke:
    DUGITE_DUAL_DECODE=panic \
    cargo nextest run \
      -p dugite-serialization \
      --features dugite-serialization/pallas-shadow-decode \
      --no-fail-fast

# Run the dual-decode soak on NETWORK (preview|preprod|mainnet|devnet).
# Pass MAX_BLOCKS as second arg to limit blocks applied (0 = unlimited).
# Accepts extra flags: --with-mithril
dual-decode-soak NETWORK="preview" MAX_BLOCKS="0" *FLAGS="":
    ./scripts/validation/dual-decode-soak.sh {{NETWORK}} {{MAX_BLOCKS}} {{FLAGS}}

# Summarise mismatch artefacts in DIR (default ./dual_decode_mismatches/).
# Exits 0 if clean, 1 if mismatches present.
dual-decode-report DIR="./dual_decode_mismatches":
    python3 ./scripts/validation/dual-decode-report.py {{DIR}}

# ─── Dev / release ───────────────────────────────────────────────────────────

# Regenerate docs/src/reference/third-party-licenses.md.
licenses:
    python3 ./scripts/dev/generate-licenses.py > docs/src/reference/third-party-licenses.md

# Prune stale worktree branches.
clean-worktrees:
    ./scripts/dev/cleanup-worktree-branches.sh

# Query the connected dugite-node socket for tip info.
query-tip *args="":
    ./scripts/dev/query-tip.sh {{args}}
