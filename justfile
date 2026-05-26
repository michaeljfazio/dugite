# Dugite task runner. Install: https://github.com/casey/just
#
# Recipes are grouped: build & dev, run a network, local devnet,
# preview soak, monitoring, validation rigs, dual-decode, upstream
# conformance, dev/release. Run `just` (or `just --list`) to see them all.

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
    #!/usr/bin/env bash
    set -euo pipefail
    cd testnet/local-devnet
    latest=$(ls -t evidence 2>/dev/null | head -1)
    [ -z "$latest" ] && { echo "No evidence directories found in testnet/local-devnet/evidence/"; exit 1; }
    ./verify.sh "evidence/$latest"

# Stop all local-devnet processes.
devnet-stop:
    ./testnet/local-devnet/stop.sh

# Generate a release report from the most recent evidence directory (optional TAG).
devnet-report TAG="":
    #!/usr/bin/env bash
    set -euo pipefail
    REPO_ROOT="$(pwd)"
    cd testnet/local-devnet
    latest=$(ls -t evidence 2>/dev/null | head -1)
    [ -z "$latest" ] && { echo "No evidence directories found in testnet/local-devnet/evidence/"; exit 1; }
    tag_flag=""
    [ -n "{{TAG}}" ] && tag_flag="--tag {{TAG}}"
    mkdir -p "$REPO_ROOT/reports/devnet-validate"
    "$REPO_ROOT/.claude/skills/devnet-validate/scripts/generate-release-report.sh" \
        --preset standard \
        $tag_flag \
        --output-dir "$REPO_ROOT/reports/devnet-validate" \
        "evidence/$latest"
    echo "Report written to reports/devnet-validate/"

# Run smoke devnet-validate (single boot, ~5 min). PR gate for core crates.
devnet-validate-smoke:
    #!/usr/bin/env bash
    set -euo pipefail
    REPO_ROOT="$(pwd)"
    cd testnet/local-devnet
    ./setup.sh
    ./run.sh
    trap './stop.sh 2>/dev/null || true' EXIT
    # Wait for relay socket
    for i in $(seq 1 30); do
        sleep 2
        [ -S "/tmp/ld-$(id -u)/relay.sock" ] && break
    done
    # Wait for 3+ blocks
    for i in $(seq 1 30); do
        sleep 3
        B=$(cardano-cli query tip --testnet-magic 42 --socket-path "/tmp/ld-$(id -u)/relay.sock" 2>/dev/null | jq -r '.block // 0' || echo 0)
        [ "$B" -ge 3 ] && break
    done
    # Smoke = tx-zoo correctness + log-level predicate only.
    # verify.sh's tip-parity / tx-inclusion predicates require sustained soak
    # evidence and live in the standard/extended presets.
    EVD="evidence/smoke-$(date -u +%Y%m%dT%H%M%SZ)"
    mkdir -p "$EVD"
    EVIDENCE_DIR="$EVD" ./tx-zoo/run-all.sh 01-bookkeeping 02-native-scripts 08-negative
    EVIDENCE_DIR="$EVD" ./perf/log-level-predicate.sh
    ./stop.sh 2>/dev/null || true
    "$REPO_ROOT/.claude/skills/devnet-validate/scripts/generate-release-report.sh" \
        --preset smoke \
        --output-dir "$REPO_ROOT/reports/devnet-validate" \
        "$EVD"
    echo "Smoke report written to reports/devnet-validate/"

# Run extended devnet-validate (~75 min). Used for release tagging.
devnet-validate-extended:
    #!/usr/bin/env bash
    set -euo pipefail
    REPO_ROOT="$(pwd)"
    cd testnet/local-devnet
    # Ensure stop.sh fires on any exit path (set -e abort, ^C, normal end).
    # RETURN traps fire only on function return, not on script exit — so the
    # old `trap ... RETURN` left orphan node processes whenever a round failed.
    trap './stop.sh 2>/dev/null || true' EXIT
    ROUND_DIRS=()
    for ROUND in 1 2 3; do
        EVD="evidence/round${ROUND}-$(date -u +%Y%m%dT%H%M%SZ)"
        mkdir -p "$EVD"
        ./setup.sh
        ./run.sh
        for i in $(seq 1 30); do sleep 2; [ -S "/tmp/ld-$(id -u)/relay.sock" ] && break; done
        for i in $(seq 1 30); do
            sleep 3
            B=$(cardano-cli query tip --testnet-magic 42 --socket-path "/tmp/ld-$(id -u)/relay.sock" 2>/dev/null | jq -r '.block // 0' || echo 0)
            [ "$B" -ge 5 ] && break
        done
        EVIDENCE_DIR="$EVD" ./tx-zoo/run-all.sh
        EVIDENCE_DIR="$EVD" ./sync/bulk-sync-throughput.sh
        EVIDENCE_DIR="$EVD" ./perf/resource-health.sh
        EVIDENCE_DIR="$EVD" ./perf/log-level-predicate.sh
        # Cardano-bp can fall far behind dugite-bp during tx-zoo's
        # tx-submission bursts: dugite's chain-sync server occasionally
        # goes silent on cardano-bp's downstream connection under heavy
        # N2C query load (the recipe runs hundreds of cardano-cli queries
        # against cardano-bp during wait_all_observers polling). When the
        # chain-sync stream stops flowing, cardano-bp parks at a stale
        # block forever; restarting just cardano-bp re-establishes the
        # chain-sync intersection and lets it bulk-download the catch-up
        # via blockfetch, which is robust under the same load.
        echo "=== catch-up gate: waiting for cardano-bp tip ≤5 blocks behind dugite-bp ==="
        for ROUND_IDX in $(seq 1 90); do
            D=$(cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-$(id -u)/dbp.sock 2>/dev/null | jq -r '.block // 0')
            C=$(cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-$(id -u)/cbp.sock 2>/dev/null | jq -r '.block // 0')
            GAP=$(( D - C )); [ "$GAP" -lt 0 ] && GAP=$(( -GAP ))
            echo "  attempt=$ROUND_IDX dugite-bp=$D cardano-bp=$C gap=$GAP"
            [ "$GAP" -le 5 ] && break
            # After 60s of no progress, restart cardano-bp to clear a
            # silent chain-sync stall. The restart drops + reconnects
            # the downstream connection, re-issuing MsgFindIntersect on
            # the new socket.
            if [ "$ROUND_IDX" -eq 30 ] && [ "$GAP" -gt 50 ]; then
                echo "  cardano-bp not catching up — bouncing it"
                if [ -f state/cardano-bp.pid ]; then
                    kill "$(cat state/cardano-bp.pid)" 2>/dev/null || true
                    sleep 5
                fi
                cardano-node run \
                    --config        "config/cardano-bp.config.json" \
                    --topology      "config/cardano-bp.topology.json" \
                    --database-path "state/cardano-bp.db" \
                    --socket-path   "/tmp/ld-$(id -u)/cbp.sock" \
                    --host-addr     127.0.0.1 \
                    --port          3003 \
                    >> "logs/cardano-bp.log" 2>&1 &
                echo $! > state/cardano-bp.pid
                # Give cardano-bp time to come up and re-handshake.
                for _ in $(seq 1 20); do
                    sleep 2
                    cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-$(id -u)/cbp.sock >/dev/null 2>&1 && break
                done
            fi
            sleep 2
        done
        EVIDENCE_DIR="$EVD" ./perf/determinism-feasibility.sh
        [ "$ROUND" -eq 2 ] && {
            sleep 300  # extra wait for epoch boundary
            EVIDENCE_DIR="$EVD" ./tx-zoo/run-all.sh 10-gov-lifecycle
        }
        # Pre-soak catch-up gate — Round 2's extra gov-lifecycle re-run
        # puts cardano-bp back under load and it falls behind again,
        # which would otherwise tank p4:tip-parity (we have measured
        # 0/36 ticks in-parity here on a previous run).  Bounce cbp if
        # it is still > 50 blocks behind after 60 s so the soak's
        # tip-parity window opens with all three observers in lockstep.
        echo "=== pre-soak catch-up gate ==="
        for ROUND_IDX in $(seq 1 90); do
            D=$(cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-$(id -u)/dbp.sock 2>/dev/null | jq -r '.block // 0')
            C=$(cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-$(id -u)/cbp.sock 2>/dev/null | jq -r '.block // 0')
            GAP=$(( D - C )); [ "$GAP" -lt 0 ] && GAP=$(( -GAP ))
            echo "  attempt=$ROUND_IDX dugite-bp=$D cardano-bp=$C gap=$GAP"
            [ "$GAP" -le 5 ] && break
            if [ "$ROUND_IDX" -eq 30 ] && [ "$GAP" -gt 50 ]; then
                echo "  cardano-bp not catching up — bouncing it"
                if [ -f state/cardano-bp.pid ]; then
                    kill "$(cat state/cardano-bp.pid)" 2>/dev/null || true
                    sleep 5
                fi
                cardano-node run \
                    --config        "config/cardano-bp.config.json" \
                    --topology      "config/cardano-bp.topology.json" \
                    --database-path "state/cardano-bp.db" \
                    --socket-path   "/tmp/ld-$(id -u)/cbp.sock" \
                    --host-addr     127.0.0.1 \
                    --port          3003 \
                    >> "logs/cardano-bp.log" 2>&1 &
                echo $! > state/cardano-bp.pid
                for _ in $(seq 1 20); do
                    sleep 2
                    cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-$(id -u)/cbp.sock >/dev/null 2>&1 && break
                done
            fi
            sleep 2
        done
        # Populate the soak-style evidence (tip-samples / blocks /
        # tx-submissions / tip-age-samples) that verify.sh's p1-p5
        # predicates read — without this the predicates fail with "no-data".
        # 180s keeps the soak inside a single epoch (epochLength=400 slots
        # × 1s = 400s). The per-tick tolerance was widened from 2 to 3
        # in `verify.sh` so the natural f=0.5 propagation gaps no longer
        # trip p4.
        EVIDENCE_DIR="$EVD" ./soak.sh 180
        ./verify.sh "$EVD"
        ./stop.sh 2>/dev/null || true
        # `setup.sh` at the top of the next iteration wipes the entire
        # `evidence/` tree (it's part of $LD_EVIDENCE). Move this round's
        # output to a sibling `evidence-kept/` so the release-report
        # generator at the end can still see all 3 rounds.
        mkdir -p evidence-kept
        KEPT="evidence-kept/$(basename "$EVD")"
        cp -R "$EVD" "$KEPT"
        ROUND_DIRS+=("$KEPT")
    done
    "$REPO_ROOT/.claude/skills/devnet-validate/scripts/generate-release-report.sh" \
        --preset extended \
        --output-dir "$REPO_ROOT/reports/devnet-validate" \
        "${ROUND_DIRS[@]}"
    echo "Extended report written to reports/devnet-validate/"

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

# Start the local Prometheus + Grafana stack (Docker). Prometheus port defaults to 9090.
monitor-start prometheus_port="9090":
    PROMETHEUS_PORT={{prometheus_port}} ./scripts/monitoring/start.sh

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

# Run the dual-decode soak (NETWORK ∈ preview|preprod|mainnet|devnet; MAX_BLOCKS=0 is unlimited; FLAGS e.g. --with-mithril).
dual-decode-soak NETWORK="preview" MAX_BLOCKS="0" *FLAGS="":
    ./scripts/validation/dual-decode-soak.sh {{NETWORK}} {{MAX_BLOCKS}} {{FLAGS}}

# Summarise dual-decode mismatch artefacts (exit 0 = clean, 1 = mismatches found).
dual-decode-report DIR="./dual_decode_mismatches":
    python3 ./scripts/validation/dual-decode-report.py {{DIR}}

# ─── Upstream conformance ────────────────────────────────────────────────────
#
# Corpus republished from upstream Cardano repositories.  Pinned tag lives in
# tests/conformance/upstream/manifest.toml.  Workflow: download → run.

# Refresh vendored utxorpc/spec proto files to a specific upstream tag.
# Usage: just bump-utxorpc-spec v0.19.2
bump-utxorpc-spec TAG:
    bash scripts/dev/bump-utxorpc-spec.sh {{TAG}}

# Download every upstream fixture area at the pinned release tag.
download-upstream-fixtures:
    cargo xtask download-upstream-fixtures

# Download a single fixture area for iteration (e.g. plutus, ledger-rules, mithril).
download-upstream-fixtures-area AREA:
    cargo xtask download-upstream-fixtures --area {{AREA}}

# Run the full upstream conformance suite (UPLC + upstream_tests; reports real 0 skipped).
test-conformance: test-conformance-uplc test-conformance-upstream

# UPLC: 999 plutus-core evaluation vectors (IntersectMBO/plutus).
test-conformance-uplc:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-uplc --features upstream-conformance --test conformance

# Every upstream golden test (cardano-base, cardano-ledger, cardano-node, ledger-rules, mithril, ouroboros-consensus, fixtures-status) in one binary.
test-conformance-upstream:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests

# Regenerate the conformance corpus tarballs locally (target/conformance-corpus/).
regenerate-corpus-local:
    bash scripts/regenerate-conformance-corpus/regenerate.sh --local

# ─── Conformance per-area filters (iteration only) ───────────────────────────
#
# Each recipe filters the `upstream_tests` binary to one area's tests.  The
# "N skipped" line nextest prints here is the count of tests for OTHER areas
# that the filter excluded — NOT a coverage gap.  Use `test-conformance-upstream`
# (or `test-conformance`) for the unfiltered run that reports 0 skipped.

# cardano-base: VRF v03 / v13 test vectors (`vrf*.txt`).
test-conformance-cardano-base:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests -E 'test(/^upstream_cardano_base_/) + test(cardano_base_vrf_checks)'

# cardano-ledger: golden block/tx decode + CDDL/PParams round-trips.
test-conformance-cardano-ledger:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests -E 'test(/^upstream_cardano_ledger_/) + test(cardano_ledger_golden_decodes)'

# cardano-node: genesis-spec JSON decode (Byron + Shelley + Alonzo + Conway).
test-conformance-cardano-node:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests -E 'test(/^upstream_cardano_node_/) + test(cardano_node_genesis_decodes)'

# ledger-rules: ImpSpec replay across all Conway STS rules (~5,678 vectors).
test-conformance-ledger-rules:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests -E 'test(/^upstream_ledger_rules_/) + test(ledger_rules_imp_spec_replay)'

# mithril: certificate fixture verification.
test-conformance-mithril:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests -E 'test(/^upstream_mithril_/) + test(mithril_certificate_checks)'

# ouroboros-consensus: per-era golden block/header decode round-trips.
test-conformance-ouroboros-consensus:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests -E 'test(/^upstream_ouroboros_consensus_/) + test(ouroboros_consensus_golden_decodes)'

# fixtures-status: verify every required fixture file is present at the manifest-pinned release tag.
test-conformance-status:
    DUGITE_REQUIRE_UPSTREAM=1 cargo nextest run -p dugite-conformance --features upstream-conformance --test upstream_tests -E 'test(upstream_fixtures_status)'

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
