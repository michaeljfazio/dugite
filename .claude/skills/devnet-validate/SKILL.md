---
name: devnet-validate
description: End-to-end validation of dugite-node and dugite-cli against cardano-node on the local 3-node devnet (dugite-bp ↔ dugite-relay ↔ cardano-relay). Use when the user asks to run integration tests, validate a build before release, exercise the transaction surface (good + bad txs across every era/type), cross-validate dugite forging against Haskell, soak-test, or sanity-check changes touching ledger/consensus/network/forging/CLI. Orchestrates testnet/local-devnet (setup → run → tx-zoo → evidence → verify) across multiple configuration rounds and produces a single pass/fail report. NOT for unit tests (use `just test`) or public-testnet soaks (use scripts/soak).
---

# devnet-validate

End-to-end validation harness that exercises dugite on a local 3-node devnet, cross-validating every behaviour against a co-running cardano-node 11.0.1.

## Topology

```
   dugite-bp (forger)  ── N2N ──▶  dugite-relay  ── N2N ──▶  cardano-relay
     :3000 :12798                    :3001 :12799              :3002 (Haskell)
```

The user-facing names map to scripts/configs as follows:
- `dugite-bp` (sole forger, holds 95%+ stake)
- `dugite-relay` (middle hop, no keys)
- `cardano-relay` — implemented as `cardano-bp` in scripts, but configured as a non-forging relay (no `--shelley-{kes,vrf,operational-certificate}` flags). It is the Haskell validator.

## When to invoke

Trigger this skill when the user wants to:
- Validate a build before tagging a release
- Confirm a change to ledger / consensus / network / forging / mempool / dugite-cli didn't regress integration behaviour
- Exercise the full transaction surface (every Conway tx class, accept and reject)
- Cross-validate dugite forging: every dugite-forged block must be re-applied by Haskell ledger
- Soak test for stability over 10–20 minutes

Do NOT invoke for unit tests (`just test`), public-testnet soak (`scripts/soak/`), or single CLI smoke tests.

## Devnet parameters (committed defaults)

`testnet/local-devnet/config/spec/shelley-spec.json` is tuned for fast iteration:

| Parameter | Value | Effect |
|---|---|---|
| `slotLength` | 1.0 s | Real-time slot pacing |
| `epochLength` | 400 slots | ~6.7 min per epoch |
| `activeSlotsCoeff` | 0.5 | ~200 expected blocks per epoch |
| `securityParam` (k) | 60 | k=60, 3k/f = 360 ≤ 400 (stable) |

A 7-minute round crosses exactly one epoch boundary. See `references/parameters.md` for the math and how to override per-run.

## Prerequisites — verify before starting

```bash
command -v cardano-node && cardano-node --version | head -1   # MUST be 11.0.1+
command -v cardano-cli  && cardano-cli  --version | head -1
command -v jq curl lsof
test -x ./target/release/dugite-node || cargo build --release -p dugite-node

# Ports must be free
for p in 3000 3001 3002 12798 12799 12800; do
  if lsof -iTCP:$p -sTCP:LISTEN -P -n 2>/dev/null | grep -q LISTEN; then
    echo "PORT BUSY: $p"; exit 1
  fi
done
```

If `cardano-node` is older than 11.0.1, abort: PV10 conway-genesis rejects it. See `references/troubleshooting.md`.

## Workflow — three rounds in under 20 minutes

Use TodoWrite to track each round as `in_progress` / `completed`. Each round must end with `./stop.sh` and a fresh `./setup.sh` before the next.

### Round 1 — Baseline (~7 min)

Goal: happy path. Fresh setup, all three nodes up, dugite-bp forges canonical blocks, cardano-relay accepts every one, dugite-cli works against both sockets, the full transaction surface passes.

```bash
cd testnet/local-devnet
./setup.sh                          # ~30s — fresh genesis
./run.sh                            # ~5s — staggered start (relay → cardano → dugite-bp)
sleep 30                            # let the chain advance past slot 0
./tx-zoo/run-all.sh --setup         # ~20s — keys + plutus binaries (one-time per setup)
./tx-zoo/run-all.sh                 # ~3-5 min — all 59 tx scripts
./tx-zoo/09-cli-parity/run.sh       # ~1 min — 22 LSQ parity checks; writes cli-parity.csv
./soak.sh 120                       # 2 min idle evidence
./verify.sh evidence/$(ls -t evidence | head -1)
.claude/skills/devnet-validate/scripts/analyze-evidence.sh evidence/$(ls -t evidence | head -1)
./stop.sh
```

While the soak runs, sample monitoring in another shell (see `references/monitoring.md`):
```bash
curl -s localhost:12798/metrics | grep -E 'dugite_tip|dugite_chain_density|dugite_forge'
tail -F logs/dugite-bp.log    | grep -E 'forge|reject|ERROR'
tail -F logs/cardano-bp.log   | grep -E 'TraceAdoptedBlock|TraceForgedInvalidBlock|Error'
```

**Round 1 PASSES iff** all of:
- `tx-zoo/state/results.csv` shows ≥58/59 PASS (one V3 spend may fail without `aiken` — see `references/tx-coverage.md`)
- `verify.sh` reports 4/4 predicates pass
- Zero `TraceForgedInvalidBlock` in `logs/cardano-bp.log`
- `dugite_tip_age_seconds` stays <5 throughout the soak
- `analyze-evidence.sh` reports no anomalies
- `evidence/<ts>/cli-parity.csv` has zero DIVERGENT rows that are not filed as known-divergence issues

### Round 2 — Epoch-boundary stress (~7 min)

Goal: catch bugs that only manifest at epoch transitions — RUPD, snapshot rotation, KES rollover, leader-schedule recompute. With `epochLength=400`, a 7-min round crosses one boundary.

```bash
cd testnet/local-devnet
./setup.sh
./run.sh
sleep 30
./tx-zoo/run-all.sh --setup

# Submit a constant tx trickle so the boundary fires under load
( while true; do
    ./tx-zoo/01-bookkeeping/01a-send-lovelace.sh >/dev/null 2>&1
    sleep 20
  done ) &
TRICKLE=$!

./soak.sh 420                       # 7 min — covers one full epoch
kill $TRICKLE 2>/dev/null
./verify.sh evidence/$(ls -t evidence | head -1)
.claude/skills/devnet-validate/scripts/analyze-evidence.sh evidence/$(ls -t evidence | head -1)
./stop.sh
```

**Round 2 PASSES iff** Round 1 criteria still hold, AND:
- `logs/dugite-bp.log` shows ≥1 `epoch transition` or `EpochTransition` event
- `logs/cardano-bp.log` shows `TraceAdoptedBlock` for at least 5 post-boundary blocks
- No `RUPD`, `pulser`, `reward calculation` errors in `logs/dugite-bp.log`
- `dugite_chain_density` metric stays within ±20% of 0.5

### Round 3 — Restart resilience (~5 min)

Goal: prove dugite-bp can be stopped mid-run and rejoin the chain without divergence or the stale-intersection bug.

```bash
cd testnet/local-devnet
./setup.sh
./run.sh
sleep 60
TIP_BEFORE=$(cardano-cli query tip --testnet-magic 42 --socket-path state/dugite-bp.sock | jq -r .block)
kill "$(cat state/dugite-bp.pid)"
sleep 90                            # 90s offline — relay's chain advances
.claude/skills/devnet-validate/scripts/restart-dugite-bp.sh
sleep 60
TIP_AFTER=$(cardano-cli query tip --testnet-magic 42 --socket-path state/dugite-bp.sock | jq -r .block)
[ "$TIP_AFTER" -gt "$TIP_BEFORE" ] || { echo "RESTART FAIL: chain did not advance"; exit 1; }
./soak.sh 60
./verify.sh evidence/$(ls -t evidence | head -1)
.claude/skills/devnet-validate/scripts/analyze-evidence.sh evidence/$(ls -t evidence | head -1)
./stop.sh
```

**Round 3 PASSES iff**:
- After restart, dugite-bp's tip catches up within 60s (`TIP_AFTER > TIP_BEFORE`)
- No persistent `stale intersection` warning past the catch-up window
- `dugite_tip_age_seconds` returns to <5 within 60s of restart
- `verify.sh` p1 (forge cross-check) shows zero canonical blocks with missing observers

If Round 3 stalls past 60s, suspect the stale-intersection bug (memory: `project_stale_intersection_when_peer_behind`). Capture logs + metrics + evidence and report.

## Final report

After all rounds complete, generate a machine-parseable + GitHub-release-ready report:

```bash
cd testnet/local-devnet

# Collect the evidence directories for each completed round (most-recent first)
EVD_ROUND3=$(ls -t evidence | sed -n '1p')
EVD_ROUND2=$(ls -t evidence | sed -n '2p')
EVD_ROUND1=$(ls -t evidence | sed -n '3p')

# Optionally pass the previous release report for trend comparison:
# --previous-report ../../reports/devnet-validate/v1.7.0.json

../../.claude/skills/devnet-validate/scripts/generate-release-report.sh \
    --preset standard \
    --round-names "baseline,epoch-boundary,restart" \
    --tx-zoo-state tx-zoo/state \
    --output-dir ../../reports/devnet-validate \
    "evidence/$EVD_ROUND1" "evidence/$EVD_ROUND2" "evidence/$EVD_ROUND3"
```

This writes two files:
- `reports/devnet-validate/report.json` — schema-versioned, suitable for trend tracking and CI diffing
- `reports/devnet-validate/report.md` — paste directly into the GitHub release body

Or use the justfile shortcut:
```bash
just devnet-report v1.8.0
```

**Storing reports**: When tagging a release, commit `reports/devnet-validate/<tag>.json` to `main` before pushing the tag. Attach both `<tag>.json` and the three binary tarballs as GitHub release assets (see `.claude/skills/release-lead/SKILL.md` for the full release checklist).

If any round fails, stop. Do not run the next round. Bundle `logs/` + `evidence/<ts>/` + `tx-zoo/state/` and produce a forensic report (commit hash, exact failing predicate, log excerpts, metric snapshot at failure time).

## Reference files (read on demand)

- `references/parameters.md` — slot/epoch/security math, override mechanics, what the values mean
- `references/monitoring.md` — log patterns, prometheus metric meanings, healthy-vs-sick examples
- `references/tx-coverage.md` — tx-zoo categories, what each script proves, expected pass/fail
- `references/cross-validation.md` — evidence file schemas, verify.sh predicate semantics
- `references/troubleshooting.md` — every known gotcha (stale intersection, KES, ports, genesis staleness, App Nap)

## Bundled scripts

- `scripts/restart-dugite-bp.sh` — relaunch ONLY dugite-bp with the same flags `run.sh` used (Round 3)
- `scripts/analyze-evidence.sh` — post-run anomaly scanner; converts an `evidence/<ts>/` directory into a plain-text anomaly report with exit-code gate
- `scripts/generate-release-report.sh` — aggregates one or more evidence directories into `report.json` + `report.md`; suitable for release gates and trend tracking. See `schemas/report.v1.json` for the output schema.

## Hard rules

- Never run two rounds without `./stop.sh` and a fresh `./setup.sh` between them — genesis must be <5min old or run.sh refuses to start.
- Always invoke `cardano-cli` against the dugite socket to test N2C compat — never the reverse (memory: `feedback_n2c_compat_test_direction`).
- Rebuild before declaring a fix unfixed — check `target/release/dugite-node` mtime vs the fix commit time (memory: `feedback_rebuild_before_declaring_unfixed`).
- A PASS requires on-disk evidence. Do not call PASS from log skim alone — quote `verify.sh` output verbatim.
- Treat unexpected log lines as load-bearing signals, not noise. The point of this skill is to find bugs.
