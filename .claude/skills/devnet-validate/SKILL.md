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
| `securityParam` (k) | 40 | 3k/f=240 ≤ 400 (stable); 4k/f=320 ≤ 400 (RUPD pulser fits) |

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

## Test methodology — coverage axes at a glance

Before running rounds, understand what each round contributes to the overall coverage charter. The skill exercises six orthogonal axes (see `references/test-methodology.md` for the full catalogue):

1. **Tx-type** — every Conway tx class (85 zoo scripts spanning bookkeeping, native, Plutus V1/V2/V3, stake, gov certs, gov proposals, voting, gov lifecycle, mempool, post-enactment — the 19 phase-1 negatives are among them).
2. **Validity** — every positive class has a matched negative; both must be classified identically by dugite and Haskell.
3. **Submit-path** — txs submitted to **every** N2C ingestion socket: `dugite-bp.sock`, `dugite-relay.sock`, `cardano-bp.sock` (override via `ZOO_SOCKET=...`), plus `dugite-cli` vs `cardano-cli` on each. Also the UTxO RPC gRPC `submit_tx` (when `--rpc-port` is enabled).
4. **Propagation-direction** — observe each tx at every node (mempool + ledger), in both forward (dugite-bp → relay → cardano-bp) and reverse (cardano-bp → relay → dugite-bp) directions through the hub.
5. **Actor** — good-actor inputs (zoo positives, cli parity) AND bad-actor inputs (zoo negatives, `protocols/` adversarial framing, `chaos/` failure injection, RPC oversized/replay/flood).
6. **Workload** — quiescent (Round 1 soak), trickle (Round 2 boundary), restart (Round 3), saturation + concurrent-burst + adversarial (Round 4 — see methodology doc).

The **bidirectional parity oracle** is the most important predicate this skill
enforces: *for every transaction T, dugite and Haskell must reach the same
accept/reject decision regardless of which node ingested it first.* Off-diagonal
cells (one accepts, the other rejects) are P0 bugs. It covers **79 of the 85 zoo
scripts** (9 categories); the matrix lands in `evidence/<ts>/parity-matrix.csv`
with a `parity-matrix.meta.json` sidecar carrying the denominator the invocation
intended to cover.

Matrix verdicts:

| Verdict | Meaning | Fails the run? |
|---|---|---|
| `MATCH` | same accept/reject decision, same reason | — |
| `OFFDIAG` | one node accepts what the other rejects | **yes (P0)** |
| `CLASSDIFF` | both reject, but for different reasons | **yes (P2)** |
| `KNOWNDIFF` | reject-reason difference that is documented and deliberate | no |
| `STATEFUL` | excluded: the subject is a global devnet resource the first batch already mutated | no |

Only reject **reasons** are compared, never accepted-tx details — each batch
runs with its own keys, so minted policy ids, script addresses and reference
txids legitimately differ and comparing them produced 7 false `CLASSDIFF`s on
the first full run.

The 6 uncovered scripts are `10-gov-lifecycle` (5) and `12-post-enactment` (1):
ratification is a property of the whole chain rather than of a batch, so a
second batch's enactment assertion is not independent. Plus `05g`/`05h`
(constitutional-committee certs) run but are recorded `STATEFUL` — the committee
is seated at genesis, so once batch 1 resigns `cc-1`, batch 2 correctly gets
`ConwayCommitteeHasPreviouslyResigned`. All exclusions and their reasons are in
`schemas/denominators.json`, and `test-denominators.sh` asserts that every
category is either required or explicitly excluded.

In addition to tx and N2N coverage, the skill exercises **dugite-cli surface parity** (today `09-cli-parity/` covers 22 query subcommands — see methodology doc for the full surface and gaps) and **UTxO RPC gRPC coverage** (Query / Submit / Sync / Watch services across v1alpha + v1beta; RPC is currently not wired into the devnet `run.sh` — opening this gap is tracked in the coverage-debt checklist).

## Workflow — three rounds in under 20 minutes

Use TodoWrite to track each round as `in_progress` / `completed`. Each round must end with `./stop.sh` and a fresh `./setup.sh` before the next.

### Round 1 — Baseline (~7 min)

Goal: happy path. Fresh setup, all three nodes up, dugite-bp forges canonical blocks, cardano-relay accepts every one, dugite-cli works against both sockets, the full transaction surface passes.

**Pin ONE evidence directory for the whole round.** `09-cli-parity/run.sh`,
`protocols/run.sh` and `bidirectional-parity.sh` all default to "newest
directory under `evidence/`". `soak.sh` *creates* a new one. So whether those
suites land beside the soak evidence or in a directory of their own depends
purely on the order they happen to run in — and a suite that wrote to the other
directory is indistinguishable, to the report generator, from a suite that never
ran. Pass the directory explicitly and the ambiguity disappears.

```bash
cd testnet/local-devnet
. ./lib/common.sh                   # exports LD_RELAY_SOCK, LD_CARDANO_BP_SOCK, LD_DUGITE_BP_SOCK
./setup.sh                          # ~30s — fresh genesis
./run.sh                            # ~5s — staggered start (relay → cardano → dugite-bp)
sleep 30                            # let the chain advance past slot 0

EVD="$LD_EVIDENCE/round1-$(date -u +%Y%m%dT%H%M%SZ)"; mkdir -p "$EVD"

./tx-zoo/run-all.sh --setup         # ~20s — keys + plutus binaries (one-time per setup)
EVIDENCE_DIR="$EVD" ./tx-zoo/run-all.sh   # ~3-5 min — all 85 tx scripts (via dugite-relay socket)
# Bidirectional parity — re-run against the Haskell socket too (catches
# accept-set asymmetry; see references/test-methodology.md "parity oracle").
# Writes parity-matrix.csv + parity-matrix.meta.json (the meta carries the
# denominator this invocation intended to cover).
# With no categories named it uses the STANDARD set from
# schemas/denominators.json (9 categories, 79 scripts). Naming categories at
# the call site is how it stayed at 4 categories / 41 scripts while the notes
# said "41/41" without ever stating the zoo has 85 (#954).
../../.claude/skills/devnet-validate/scripts/bidirectional-parity.sh \
    --out "$EVD/parity-matrix.csv"
./tx-zoo/09-cli-parity/run.sh "$EVD"   # ~1 min — 22 LSQ parity checks; writes cli-parity.csv
./tx-zoo/cross-validate-cli.sh         # ~1 min — dugite-cli ↔ cardano-cli submit parity
./protocols/run.sh "$EVD"              # ~2 min — adversarial N2N framing; writes n2n-trace.csv
./chaos/run.sh "$EVD"                  # ~3 min — kill-9 recovery + app-nap + clock-skew
                                       #          + syn-flood; writes chaos-events.csv
EVIDENCE_DIR="$EVD" ./soak.sh 120      # 2 min idle evidence
./verify.sh "$EVD"
../../.claude/skills/devnet-validate/scripts/analyze-evidence.sh "$EVD"
./stop.sh
```

While the soak runs, sample monitoring in another shell. Two complementary scripts:

```bash
# (a) Fast one-shot health verdict — recommended every ≤60s during a soak. Implements
# the 14-step decision procedure in references/health.md. Exits non-zero on any anomaly.
# When invoked from inside testnet/local-devnet/ the log auto-locate works without args;
# from anywhere else, pass --log / --relay-log / --cardano-log explicitly.
../../.claude/skills/devnet-validate/scripts/health-probe.sh --verbose

# (b) Comprehensive metric audit — run once after warmup AND once at end-of-round. Validates
# all ~70 metrics dugite-monitor consumes plus cross-node + Haskell parity. Use --verbose to
# see each assertion pass. Hard-fails on any invariant violation.
../../.claude/skills/devnet-validate/scripts/metric-audit.sh --verbose

# Raw streams (fall back when the probe isn't enough):
curl -s localhost:12798/metrics | grep -E '^dugite_(tip|block|slot|peers|forge|chainsync)'
tail -F logs/dugite-bp.log    | grep -E 'forge|reject|ERROR|stale'
# cardano-node 11.x uses new-tracer namespace; match both legacy + new names.
tail -F logs/cardano-bp.log   | grep -E 'AddedToCurrentChain|AddBlockValidation\.InvalidBlock|Forge\.Loop\.ForgedInvalidBlock|TraceAdoptedBlock|TraceForgedInvalidBlock|Mempool\.AddedTx|MempoolAccepted|mismatched|timeout'
```

**Round 1 PASSES iff** all of:
- `tx-zoo/state/results.csv` shows 85 rows with 0 FAIL (`04g-reward-withdrawal` state-skips until rewards mature — see #958; V3 scripts need `aiken` on PATH)
- `verify.sh` reports 4/4 predicates pass
- Zero invalid-block events in `logs/cardano-bp.log` (match BOTH legacy `TraceForgedInvalidBlock` and cardano-node 11.x `ChainDB.AddBlockEvent.AddBlockValidation.InvalidBlock` / `Forge.Loop.ForgedInvalidBlock`)
- `dugite_tip_age_seconds` stays <5 throughout the soak
- `health-probe.sh` returns HEALTHY at end-of-round AND at every ≤60s sample during the soak (network throughput + Haskell-tip parity included)
- `metric-audit.sh` exits 0 at end-of-round (all ~30 metric assertions pass: completeness, arithmetic invariants, counter monotonicity, BP↔relay parity, Haskell parity, range checks)
- `analyze-evidence.sh` reports no anomalies
- `evidence/<ts>/cli-parity.csv` has zero DIVERGENT rows that are not filed as known-divergence issues, **and zero ERROR rows** (`09-cli-parity/run.sh` now exits 1 on either). An ERROR row noted `HARNESS both-sides-failed` means the suite passed cardano-cli arguments it does not accept — fix the `09*.sh` script, do not add it to `KNOWN_DIVERGENCES`
- `evidence/<ts>/n2n-trace.csv` has zero PANIC or SILENT_SKIP rows
- `evidence/<ts>/chaos-events.csv` has zero FAIL rows, and any `ENV_SKIP` row is
  a surface that was **not** exercised — investigate rather than accept it
- Bidirectional parity wrapper (`bidirectional-parity.sh`, no args = the pinned standard set of 9 categories / 79 scripts) exits 0 — zero `OFFDIAG` and zero unexplained `CLASSDIFF` rows in `evidence/<ts>/parity-matrix.csv`
- `tx-zoo/state/cross-validate.csv` shows PASS for every representative tx submitted through `dugite-cli`

### Round 2 — Epoch-boundary stress (~15 min)

Goal: catch bugs that only manifest at epoch transitions — RUPD, snapshot rotation, KES rollover, leader-schedule recompute, and the **first reward-pot movement** (treasury↑ / reserves↓). With `epochLength=400`, `k=40`, `f=0.5`, the Praos randomness-stabilisation anchor is `4k/f = 320 ≤ 400`, so the RUPD pulser starts at slot 320 of each epoch and finishes before the boundary. Rewards computed in epoch *N* are applied at boundary *N+1 → N+2*. The first non-zero RUPD therefore lands at boundary **1→2 (slot 800)**. A 15-min soak crosses both boundaries with margin so the pot movement can be observed and byte-exactly cross-validated against Haskell.

```bash
cd testnet/local-devnet
./setup.sh
./run.sh
sleep 30
./tx-zoo/run-all.sh --setup

# Submit a constant tx trickle so the boundaries fire under load (and produce fees)
( while true; do
    ./tx-zoo/01-bookkeeping/01a-simple-pay.sh >/dev/null 2>&1
    sleep 20
  done ) &
TRICKLE=$!

./soak.sh 900                       # 15 min — covers boundaries 0→1 AND 1→2 (first RUPD)
kill $TRICKLE 2>/dev/null

# Pot-movement parity check at end (must be post-boundary 1→2, i.e. epoch >= 2)
DBP_T=$(curl -s localhost:12798/metrics | awk '/^dugite_treasury_lovelace /{print $2}')
DBP_R=$(curl -s localhost:12798/metrics | awk '/^dugite_reserves_lovelace /{print $2}')
HSK=$(cardano-cli query ledger-state --testnet-magic 42 --socket-path "/tmp/ld-$UID/cbp.sock" \
        | jq '.stateBefore.esChainAccountState // .esChainAccountState')
HSK_T=$(echo "$HSK" | jq -r .treasury)
HSK_R=$(echo "$HSK" | jq -r .reserves)
echo "dugite treasury=$DBP_T reserves=$DBP_R"
echo "haskell treasury=$HSK_T reserves=$HSK_R"
[ "$DBP_T" = "$HSK_T" ] && [ "$DBP_R" = "$HSK_R" ] || { echo "POT PARITY FAIL"; exit 1; }
[ "$DBP_T" -gt 0 ] || { echo "RUPD DIDN'T APPLY: treasury still 0 after boundary 1→2"; exit 1; }

./verify.sh evidence/$(ls -t evidence | head -1)
.claude/skills/devnet-validate/scripts/analyze-evidence.sh evidence/$(ls -t evidence | head -1)
./stop.sh
```

**Round 2 PASSES iff** Round 1 criteria still hold, AND:
- Final tip epoch ≥ 2 (boundary 1→2 was crossed) — query via `cardano-cli query tip --testnet-magic 42 --socket-path /tmp/ld-$UID/dbp.sock`
- `logs/cardano-bp.log` shows `TraceAdoptedBlock` for at least 5 post-boundary blocks (per boundary)
- No `RUPD`, `pulser`, `reward calculation` errors in `logs/dugite-bp.log`
- `analyze-evidence.sh` chain-density proxy (canonical blocks ÷ slots) stays within ±20% of `activeSlotsCoeff` (0.5 on devnet)
- After boundary 1→2: `dugite_treasury_lovelace > 0` AND `dugite_reserves_lovelace < genesis_reserves` (RUPD applied)
- `dugite_treasury_lovelace` **byte-exactly equals** `cardano-bp.esChainAccountState.treasury` AND `dugite_reserves_lovelace` **byte-exactly equals** `cardano-bp.esChainAccountState.reserves` (the only acceptable ledger semantic per `feedback_haskell_byte_exact_only`)
- Boundary 0→1 having `treasury=0, reserves=genesis` is **expected and correct**: epoch 0's pulser runs (anchor `4k/f=320` fits inside `epoch_len=400`) but applies at boundary 1→2, not 0→1; the pot-movement check applies post-1→2 only

### Round 3 — Restart resilience (~5 min)

Goal: prove dugite-bp can be stopped mid-run and rejoin the chain without divergence or the stale-intersection bug.

```bash
cd testnet/local-devnet
./setup.sh
./run.sh
sleep 60
TIP_BEFORE=$(cardano-cli query tip --testnet-magic 42 --socket-path "$LD_DUGITE_BP_SOCK" | jq -r .block)
kill "$(cat state/dugite-bp.pid)"
sleep 90                            # 90s offline — relay's chain advances
.claude/skills/devnet-validate/scripts/restart-dugite-bp.sh
sleep 60
TIP_AFTER=$(cardano-cli query tip --testnet-magic 42 --socket-path "$LD_DUGITE_BP_SOCK" | jq -r .block)
# Fail loudly when the criterion could not be MEASURED — an empty tip must not
# be reported the same way as a tip that did not advance (#944).
[ -n "$TIP_BEFORE" ] || { echo "RESTART INCONCLUSIVE: could not read tip before restart"; exit 1; }
[ -n "$TIP_AFTER" ]  || { echo "RESTART INCONCLUSIVE: could not read tip after restart"; exit 1; }
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
- `dugite_chainsync_idle_seconds` returns to <4 within 60s of restart
- `health-probe.sh` reports HEALTHY 60s post-restart (also covers Haskell-tip parity + cardano-bp adoption resumed)
- `verify.sh` p1 (forge cross-check) shows zero canonical blocks with missing observers

If Round 3 stalls past 60s, suspect the stale-intersection bug (memory: `project_stale_intersection_when_peer_behind`). Capture logs + metrics + evidence and report.

## Final report

After all rounds complete, generate a machine-parseable + GitHub-release-ready report:

Collecting the rounds needs care for two reasons:

1. **Only the CURRENT round lives in `evidence/`.** Each round starts with
   `./setup.sh`, which moves the previous round's evidence into
   `evidence-archive/auto/` (it used to delete it, which made every round but
   the last unreportable). So after three rounds you have one directory in
   `evidence/` and the earlier two in `evidence-archive/auto/`.
2. **A round can produce more than one evidence directory.** `09-cli-parity/`
   and `protocols/` create their own before the soak does. Those hold
   `cli-parity.csv` / `n2n-trace.csv` but no `report.md`, and must not be
   counted as rounds.

The reliable predicate is therefore "directory containing `report.md`" — that
file is written by `verify.sh`, once per round.

```bash
cd testnet/local-devnet

# Reportable round dirs across both locations, oldest first, last 3 only.
# Basenames are ISO-8601, so a lexicographic sort is chronological, which for a
# sequential run is round order.
ROUND_DIRS=$(
  for d in evidence/*/ evidence-archive/auto/*/; do
    [ -d "$d" ] && [ -f "${d}report.md" ] && printf '%s\t%s\n' "$(basename "$d")" "${d%/}"
  done | sort | cut -f2 | tail -3
)
EVD_ROUND1=$(echo "$ROUND_DIRS" | sed -n '1p')
EVD_ROUND2=$(echo "$ROUND_DIRS" | sed -n '2p')
EVD_ROUND3=$(echo "$ROUND_DIRS" | sed -n '3p')
[ -n "$EVD_ROUND1" ] && [ -n "$EVD_ROUND2" ] && [ -n "$EVD_ROUND3" ] || {
    echo "expected 3 rounds with report.md, got:"; echo "$ROUND_DIRS"; exit 1; }

# Optionally pass the previous release report for trend comparison:
# --previous-report ../../reports/devnet-validate/v2.1.0.json

# NB: these are full paths already — do NOT prefix them with evidence/.
../../.claude/skills/devnet-validate/scripts/generate-release-report.sh \
    --preset standard \
    --round-names "baseline,epoch-boundary,restart" \
    --tx-zoo-state tx-zoo/state \
    --output-dir ../../reports/devnet-validate \
    "$EVD_ROUND1" "$EVD_ROUND2" "$EVD_ROUND3"
```

Sanity-check the result before shipping it: each round's `tx_zoo.source` in
`report.json` should be `"round"` (its own snapshot), not `"shared"`. A
`"shared"` source means `soak.sh` did not snapshot `tx-results.csv` for that
round, and the per-round transaction counts are the cumulative ledger rather
than that round's.

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

- `references/test-methodology.md` — **start here for any coverage question**. Six orthogonal coverage axes (tx-type, validity, submit-path, propagation-direction, actor, workload), the bidirectional accept/reject parity oracle, full dugite-cli surface map, UTxO RPC (gRPC) coverage matrix, era / governance / stress recipes, and the running coverage-debt checklist.
- `references/health.md` — **start here for any runtime-health question**. Six-question health model, lifecycle phases (boot/catch-up/at-tip/boundary/restart), healthy ranges, network-performance signals, log-sampling cadence, Haskell cross-validation, and the 14-step evaluation procedure that `health-probe.sh` implements.
- `references/monitoring.md` — authoritative catalog of dugite Prometheus metrics + log patterns (the "what does this name mean" lookup, verified against the actual code)
- `references/parameters.md` — slot/epoch/security math, override mechanics, what the values mean
- `references/tx-coverage.md` — tx-zoo categories, what each script proves, expected pass/fail
- `references/cross-validation.md` — evidence file schemas, verify.sh predicate semantics
- `references/troubleshooting.md` — every known gotcha (stale intersection, KES, ports, genesis staleness, App Nap)

## Bundled scripts

- `scripts/health-probe.sh` — one-shot runtime health verdict (process + Prometheus + wall-clock + peers + tip + apply + forge + snapshot + network deltas + connection thrash + Haskell-tip parity + recent Haskell adoption + log delta + cross-validation). Exits 0 = HEALTHY, non-zero = SICK with anomaly list. Use during soaks and Ralph loops; `--public` relaxes thresholds for public testnets.
- `scripts/metric-audit.sh` — full metric validation pass. Reads every metric dugite-monitor consumes (~70 metrics) and runs assertions in 6 phases: (1) completeness — every dugite-monitor metric is exposed; (2) per-node arithmetic invariants (e.g. `peers_connected == peers_hot + peers_warm`, `peers_inbound + peers_outbound - conn_full_duplex == peers_connected`); (3) counter monotonicity over a 3s window; (4) BP↔relay cross-node consistency (block-number parity, forge↔receive balance, role assignment); (5) Haskell parity via `cardano-cli` socket query; (6) range checks on tip-age, mempool, disk, snapshot worker, ledger pots. Use after warmup to validate that every gauge the human operator inspects matches expectations.
- `scripts/restart-dugite-bp.sh` — relaunch ONLY dugite-bp with the same flags `run.sh` used (Round 3)
- `scripts/analyze-evidence.sh` — post-run anomaly scanner; converts an `evidence/<ts>/` directory into a plain-text anomaly report with exit-code gate
- `scripts/generate-release-report.sh` — aggregates one or more evidence directories into `report.json` + `report.md`; suitable for release gates and trend tracking. See `schemas/report.v1.json` for the output schema.

## Hard rules

- Never run two rounds without `./stop.sh` and a fresh `./setup.sh` between them — genesis must be <5min old or run.sh refuses to start.
- Always invoke `cardano-cli` against the dugite socket to test N2C compat — never the reverse (memory: `feedback_n2c_compat_test_direction`).
- Rebuild before declaring a fix unfixed — check `target/release/dugite-node` mtime vs the fix commit time (memory: `feedback_rebuild_before_declaring_unfixed`).
- A PASS requires on-disk evidence. Do not call PASS from log skim alone — quote `verify.sh` output verbatim.
- Treat unexpected log lines as load-bearing signals, not noise. The point of this skill is to find bugs.

---

## v2 capability matrix (added Phases 1-8)

The harness now covers 9 dimensions across 3 intensity presets:

| Dim | Capability | Smoke | Standard | Extended |
|-----|------------|-------|----------|----------|
| D1 | Forge & adoption | 1 epoch | 2 epochs | 5 epochs |
| D2 | Tx surface | 08-negative subset | 85 scripts (19 negatives) + parity oracle over 79 | + CBOR fuzz |
| D3 | N2N adversarial | handshake only | + all 7 protocol scripts | + slow-loris |
| D4 | N2C CLI parity | 3 queries | 22 queries (09a–09v), all compared | all |
| D5 | Sync paths | from-relay-tip | + bulk-throughput | + from-genesis + Mithril |
| D6 | Chaos | — | kill-9 + app-nap + clock-skew + syn-flood | + partition + disk-full |
| D7 | Epoch transitions | 1 boundary | 2 boundaries | + gov-lifecycle enactment |
| D8 | Resource health | log-level only | + CPU/RSS/FD sampling | + 30-min leak check |
| D9 | Determinism | — | feasibility verdict | tip-hash match |

### Quick-start v2

```bash
# Smoke (~5 min) — PR gate for core crates
just devnet-validate-smoke

# Standard (~20 min) — THE RELEASE TAG GATE.
# Run the Round 1-3 workflow above, then the "Final report" block.
# `just devnet-report` is NOT this: it reports a single round from whatever
# evidence is lying around and marks itself gate_integrity.admissible=false.

# Extended (~75 min) — deeper pre-major-release pass, not the tag gate
just devnet-validate-extended

# Reporting-layer self-test (~10s, no devnet) — run after editing this skill
just devnet-gate-selftest
```

### Which preset gates a tag

**Standard.** `.claude/skills/release-lead/SKILL.md` is authoritative and says
the same. Before #953 this document nominated *extended* as the tag gate while
every command in both skills hardcoded `--preset standard` — so v2.4.3–v2.4.5
all shipped on standard and nothing extended-only ever gated a release. Rather
than leave the docs describing a gate nobody ran, standard is now the gate in
name as well as in practice, and the suites that matter most (adversarial N2N,
cli-parity, the bidirectional parity oracle, chaos) are part of the standard
evidence manifest — the generator refuses to emit a passing report without
them.

### Gate integrity — the generator fails loudly now

`generate-release-report.sh` runs **strict by default**. It hard-fails (exit 3)
when, for the declared preset, a required evidence file is absent, a suite's row
count is below its pinned denominator in
`schemas/denominators.json`, or a round's tx-zoo counts were borrowed from
another round (`source:"shared"`).

An absent suite now serializes as `status:"absent"` with **null** counts. It
never serializes as `0` — that ambiguity is what let "0 divergent" mean "never
compared" for three releases (#945, #953).

```bash
# Never do this for a release:
generate-release-report.sh --no-strict ...   # records the omission, marks
                                             # gate_integrity.admissible=false
```

Always confirm before shipping:
```bash
jq '.gate_integrity' reports/devnet-validate/report.json
# { "strict": true, "admissible": true, "missing": [] }
```

### New evidence files (v2)

| File | Written by | Content |
|------|-----------|---------|
| `cli-parity.csv` | `09-cli-parity/run.sh` | ts, query, status, dugite_sha, cardano_sha, equal, notes |
| `n2n-trace.csv` | `protocols/run.sh` | protocol, msg_type, outcome, notes |
| `throughput.csv` | `sync/*.sh` | ts, scenario, blocks, seconds, blocks/sec, MB/sec |
| `resource-samples.csv` | `perf/resource-health.sh` | ts, pid, node, cpu%, rss_kb, fds, threads |
| `chaos-events.csv` | `chaos/*.sh` | ts, scenario, action, recovery_sec, result |
| `log-anomalies.csv` | `perf/log-level-predicate.sh` | ts, node, level, pattern, count |

### New tx-zoo categories

| Category | Scripts | What it tests |
|----------|---------|---------------|
| `08-negative` (expanded) | 08e–08s (15 new) | Phase-1 predicates: NoInputs, DuplicateInput, InputNotFound, ValueNotConserved, TxTooLarge, NotYetValid, BadSignature, MissingRequiredSigner, OutputValueTooLarge, WrongNetworkOutput, InvalidMint, NativeScriptFailed, RefInputNotFound, MalformedCBOR, StakePoolCostTooLow |
| `10-gov-lifecycle` | 10a–10e | propose → DRep vote → SPO vote → CC vote → assert enactment |
| `11-mempool` | 11a–11c | TTL eviction, input-conflict rejection, drain latency p99 |

### Chaos tests

All chaos tests live in `testnet/local-devnet/chaos/`. Each records to `evidence/<ts>/chaos-events.csv`.

| Script | What it tests | Recovery bound |
|--------|--------------|---------------|
| `kill-9-mid-forge.sh` | SIGKILL recovery | 120s |
| `network-partition.sh` | 60s relay block + reconnect | 60s |
| `clock-skew.sh` | Future-slot injection rejected, no panic | — |
| `disk-full.sh` | Write failure → no corruption, no panic | — |
| `inbound-syn-flood.sh` | 200 rapid connections → node stays responsive | — |
| `macos-app-nap.sh` | `caffeinate` present and used (macOS only) | — |

### When to invoke

devnet-validate is **developer-machine only** — it is not wired into GitHub
Actions and never runs in CI.  Invoke it locally at critical moments:

- Before tagging a release (`just devnet-validate-extended` — see
  `.claude/skills/release-lead/SKILL.md` for the release checklist)
- Before merging a PR that touches ledger / consensus / network / forging /
  mempool / dugite-cli (`just devnet-validate-smoke`)
- After landing a change you want to soak-test against Haskell

CI keeps the unit/integration test surface (`just check`, `just test`)
green; devnet-validate is the human-driven cross-validation pass.

### Flaky-test policy

- **Non-chaos predicates**: zero retries. Flake = bug.
- **Chaos predicates**: 1 retry. Two consecutive failures on the same scenario = file a bug.
- **Network-bound (Mithril, public DNS)**: bounded timeout; skip-with-warning if unreachable.
