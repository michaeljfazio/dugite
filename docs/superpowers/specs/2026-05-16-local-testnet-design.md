# Local Testnet for Verification & Validation

**Date:** 2026-05-16
**Branch:** `feature/local-testnet-docs`
**Adds:** `testnet/local-devnet/` (scripts + templates), `docs/src/running/local-testnet.md`

---

## Problem

Dugite is currently validated by running against public testnets (preview, preprod) and by ad-hoc local pairings between a dugite block producer and a Haskell relay (see `scripts/launch-bp-pair*.sh`). These setups have two gaps:

1. They depend on public peers and real KES/opcert/stake registration, so they cannot be reproduced from scratch by a contributor on a clean machine, and they cannot be CI'd.
2. They exercise dugite-bp ↔ Haskell-relay but **not** dugite-**relay** ↔ Haskell-bp. The dugite relay's bidirectional ChainSync/BlockFetch path (diffusing blocks from a Haskell upstream peer to a downstream consumer that is itself talking to a different Haskell node) has never been exercised in a controlled setting.

We need a documented, reproducible local network that boots from fresh genesis in seconds, produces blocks within seconds, and exercises both directions of block diffusion across the dugite/Haskell boundary through a single dugite-relay hub.

## Goal

Land four things together:

1. A `testnet/local-devnet/` directory containing all scripts and config templates needed to bring up a 3-node loopback testnet (1 dugite BP, 1 dugite relay, 1 cardano-node BP, hub-and-spoke through the dugite relay) from a clean machine in one command.
2. A 30-minute soak test that collects machine-verifiable evidence of (a) blocks forged by both BPs ending up on a single canonical chain, (b) tip parity across all three nodes, and (c) transactions submitted to each node's N2C socket appearing in all three nodes' UTxO sets.
3. A new GitHub Pages doc page (`docs/src/running/local-testnet.md`) that is self-contained — a reader on the published site can reproduce the setup without reading the repo.
4. A GitHub tracking issue with concrete acceptance criteria, linked from the design spec and the doc page footer, that closes when the first successful soak's `report.md` is attached.

## Non-goals

- **Byron-era coverage.** The devnet boots straight into Conway PV10. Byron is exercised by mainnet syncs and Mithril import; that path is unaffected by this work.
- **Hard-fork combinator (HFC) transition testing.** No era transitions occur during the soak. (Era transitions are tested separately by mainnet/preview replay and by the Conway-bootstrap test suite.)
- **Multi-relay diffusion topology.** A single dugite-relay hub is the system under test. Testing relay→relay BlockFetch needs a separate, larger devnet.
- **Plutus phase-2 / governance enactment.** The 30-minute soak with no proposals and only payment-only txs deliberately avoids these.
- **Replacing existing soak scripts.** The preview/preprod soak rigs (`scripts/launch-bp-pair*.sh`, `scripts/soak-*.sh`) stay. This is an additional, complementary testbed.
- **Production CI integration.** The first deliverable is a manually-runnable testbed with documented results. Wiring it into GitHub Actions can come later if it proves stable.

## Design

### 1. Architecture and topology

Three processes on loopback, hub-and-spoke through the dugite relay. **No public peers anywhere**; every node sets `bootstrapPeers: null`, empty `publicRoots`, `useLedgerAfterSlot: -1`.

```
   ┌──────────────────┐        ┌────────────────────┐        ┌──────────────────────┐
   │   dugite-bp      │◄──────►│   dugite-relay     │◄──────►│   cardano-node bp    │
   │   port 30001     │  N2N   │   port 30000       │  N2N   │   port 30003         │
   │   (Rust)         │        │   (Rust)           │        │   (Haskell)          │
   │   pool1 keys     │        │   no keys          │        │   pool2 keys         │
   └──────────────────┘        └────────────────────┘        └──────────────────────┘
        N2C socket               N2C socket                       N2C socket
   dugite-bp.sock             dugite-relay.sock              cardano-bp.sock
```

The relay's `localRoots` lists both BPs (valency 2). Each BP's `localRoots` lists only the relay (valency 1). This means a block forged by dugite-bp must transit dugite-relay's BlockFetch server → cardano-bp's BlockFetch client (and vice versa) — the relay's bidirectional N2N is the system under test. All three nodes expose an N2C local socket for `cardano-cli` / `dugite-cli` queries and tx submission.

Ports 30000/30001/30003 were chosen far from existing soak defaults (3001/3002) so this devnet can run alongside an active public-network soak on the same host without conflict. The gap at 30002 leaves room to add a second relay later.

**Workspace layout** (everything new lives under `testnet/local-devnet/`, all generated artefacts gitignored):

```
testnet/local-devnet/
  setup.sh           # one-shot bootstrap: gen genesis + keys + render configs
  run.sh             # start 3 nodes (caffeinate-wrapped on macOS)
  stop.sh            # SIGTERM + cleanup; preserves DBs and logs
  soak.sh            # 30-min soak orchestrator (concurrent samplers)
  verify.sh          # parses evidence CSVs, prints pass/fail report
  submit-txs.sh      # tx-build/sign/submit helper used by soak.sh
  lib/common.sh      # shared helpers (sourced by the others)
  config/
    spec/shelley-spec.json        # genesis overrides (Section 2)
    spec/conway-spec.json
    templates/dugite-bp.config.tmpl.json
    templates/dugite-relay.config.tmpl.json
    templates/cardano-bp.config.tmpl.json
    templates/dugite-bp.topology.tmpl.json
    templates/dugite-relay.topology.tmpl.json
    templates/cardano-bp.topology.tmpl.json
  genesis/           # generated by setup.sh (gitignored)
  keys/              # generated by setup.sh (gitignored, mode 0600)
  state/             # runtime DBs + sockets (gitignored)
  logs/              # per-node logs (gitignored)
  evidence/          # soak outputs (gitignored)
  .gitignore         # excludes everything above except scripts/, config/, lib/
```

### 2. Genesis and key generation

A single `cardano-cli` invocation produces everything: genesis files, three genesis-key sets, two pool credential sets, four stake-delegator key pairs, and one UTxO-funding key pair.

```bash
cardano-cli conway genesis create-testnet-data \
  --spec-shelley     config/spec/shelley-spec.json \
  --spec-conway      config/spec/conway-spec.json \
  --testnet-magic    42 \
  --genesis-keys     3 \
  --pools            2 \
  --stake-delegators 4 \
  --utxo-keys        1 \
  --total-supply         60000000000000000 \
  --delegated-supply     30000000000000000 \
  --start-time          "$START_TIME" \
  --out-dir              genesis
```

**Why two spec overrides:** `cardano-cli` writes mainnet-shaped defaults; we patch them via spec files committed under `config/spec/`.

`shelley-spec.json` overrides:

| field | value | rationale |
|---|---|---|
| `slotLength` | `1.0` | 1-second slots |
| `activeSlotsCoeff` | `0.2` | f=0.2 — ~5s expected block time |
| `epochLength` | `500` | ~8.3 min/epoch → 3–4 epoch transitions in 30-min soak |
| `securityParam` | `10` | small k → fast immutability (3k/f = 150 slots ≈ 2.5 min) |
| `updateQuorum` | `2` | matches 3 genesis keys, 2-of-3 |
| `maxLovelaceSupply` | `60000000000000000` | mainnet-shaped, evenly divisible |
| `slotsPerKESPeriod` | `129600` | left at default — a single KES period covers entire soak |
| `networkMagic` | `42` | local devnet magic, distinct from mainnet/preview/preprod |

`conway-spec.json` override: `protocolVersion: { major: 10, minor: 0 }` so the chain boots straight into Conway PV10. Conway governance params are left at cardano-cli defaults (action lifetime 30 epochs, etc.) — they don't affect a 30-min run with no proposals.

**Pool relays on-chain:** each pool is registered at genesis with `127.0.0.1:30000` as its on-chain relay address (via the `--relays` flag pointing at a JSON file `setup.sh` writes inline). This is informational only — `localRoots` files drive runtime connections — but it means ledger-based peer discovery, if it kicks in, would not produce surprises.

**Key layout after generation:**

```
keys/
  pool1/             # dugite BP
    cold.skey, cold.vkey, cold.counter
    vrf.skey, vrf.vkey
    kes.skey, kes.vkey
    opcert.cert
  pool2/             # cardano-node BP — symmetric to pool1
  utxo/              # genesis-funded key (signs all test txs)
    payment.skey, payment.vkey
    stake.skey, stake.vkey
    payment.addr     # base address holding 30M ADA
  genesis-keys/      # 3 genesis keys (unused at runtime; kept for completeness)
```

dugite-bp launches with `--shelley-kes-key keys/pool1/kes.skey --shelley-vrf-key keys/pool1/vrf.skey --shelley-operational-certificate keys/pool1/opcert.cert`. cardano-bp uses the equivalent cardano-node config block pointing at `keys/pool2/`.

### 3. Soak test methodology and evidence

`soak.sh` runs for 30 minutes by default (configurable: `soak.sh DURATION_SECONDS`) and writes everything into `evidence/<timestamp>/`. Every assertion is computed from on-disk artefacts at the end of the run, so the same evidence can be re-derived from raw logs alone.

**Three concurrent samplers** run as background loops while the soak is alive:

| sampler | period | output | what it captures |
|---|---|---|---|
| `tip-sampler` | every 5s | `tip-samples.csv` | One row per node per tick: `ts, node, slot, block_no, hash, era`. Pulled via `cardano-cli query tip --socket-path <sock>` against each of the three N2C sockets. |
| `block-recorder` | every new block | `blocks.csv` | Tails dugite-bp's log for `Forge.AdoptedBlock` and `BlockFetch.CompletedBlockFetch` events; tails cardano-bp's log for `TraceForgedBlock`+`AdoptedBlock`. One row per first-sight: `ts, observer, slot, hash, issuer_vkey, body_size, n_txs`. |
| `tx-injector` | T+2m, T+10m, T+20m | `tx-submissions.csv` | At each tick, builds and submits **5 txs to each of the 3 sockets** (15 per wave, 45 total). Each tx is a 2 ADA self-transfer from the genesis UTxO key with a unique metadata label. Records `ts, target_socket, txid, submit_rc`. |

The tx-injector is the only sampler that interacts with the chain. The other two are read-only.

**Four pass/fail predicates** (the four evidence channels chosen during brainstorming). `verify.sh` parses the CSVs and computes each predicate. The soak fails loud (exit 1, banner) if any predicate fails.

1. **Block forge cross-check.** For every `(slot, hash)` pair in `blocks.csv`, query the block at that slot from all three nodes via `cardano-cli query ledger-state` (filtered by slot). All three must return the same hash. Tolerance: a node may be up to 2 blocks behind tip at sample time (rollback grace), but never diverge on a confirmed block (> k=10 deep).
2. **Per-BP forge attribution.** Group `blocks.csv` by `issuer_vkey`. Match against pool1's cold vkey hash (dugite) and pool2's (cardano). Both pools must have produced at least 3 blocks. Expected ~180 each at f=0.2, σ=0.5 over 1800 slots — P(zero forges for one pool) is essentially zero, so this is a real-failure detector not a luck check.
3. **Transaction inclusion (round-trip).** For each txid in `tx-submissions.csv`: after the wave completes (T+ wave + 60s), query `cardano-cli query utxo --address <change_addr>` on all three sockets. Each tx output must appear in all three UTxO sets at the same UTxO entry. Failures are bucketed by submitting socket so we know whether dugite-bp's local mempool, dugite-relay's mempool, or cardano-bp's mempool dropped it.
4. **Tip parity over time.** From `tip-samples.csv`, compute the fraction of 5s windows during which all three nodes report a tip within 2 blocks of each other. Pass threshold: ≥95% of windows are in-parity, excluding the first 60 seconds (warmup) and any 10-second window straddling a sampled rollback.

**Evidence bundle.** At the end of the run, `evidence/<timestamp>/` contains:

```
report.md              # human-readable summary (per-predicate pass/fail, counts, percentages)
tip-samples.csv        # raw 5s tip samples
blocks.csv             # one row per block first-sighted
tx-submissions.csv     # txid → submit socket, rc, included-in-block
forge-attribution.tsv  # pool1_blocks=N, pool2_blocks=M
metadata.json          # genesis hash, magic, start time, software versions
logs/                  # the three node logs (rotated copies)
```

`report.md` is the artefact attached to the GitHub issue and the PR as the concrete "evidence of blocks being forged and accepted by both" you asked for.

**Failure modes we'll guard against up front:**

- **One node never reaches the chain start.** `setup.sh` waits for all three sockets to exist and `query tip` to return a slot > 0 before declaring readiness. Hard timeout: 90s.
- **Tx submission races epoch boundary.** Genesis-key signed tx may be rejected if pparams change at epoch boundary — mitigated by submitting our self-transfer with a base address (not a stake address registration), which doesn't touch governance.
- **Wall-clock drift on macOS during soak.** Wrap `dugite-node` in `caffeinate -dimsu` (per memory `project_macos_appnap_freeze_2026_05_08.md`). Done in `run.sh`.
- **Genesis time in the past.** If `setup.sh` was run > 30s before `run.sh`, the chain would start with a backlog of empty slots. `run.sh` checks that `start-time` is within the last 5 minutes and refuses to start otherwise (re-run setup).

### 4. Documentation page

`docs/src/running/local-testnet.md` is a new mdbook page. `SUMMARY.md` is edited to add it under "Running a Node", between "Block Producer" and "Kubernetes Deployment". The page is written so it stands alone: someone reading the published GitHub Pages site can reproduce the setup without reading the repo.

Page structure:

1. **What this is** — one paragraph plus mermaid topology diagram (same shape as Section 1 above).
2. **Prerequisites** — cardano-node ≥ 11.0.1, cardano-cli ≥ 11.0.0, a built `target/release/dugite-node`, ~2 GB free disk. macOS: `caffeinate` (system-builtin).
3. **One-time setup** — `./testnet/local-devnet/setup.sh`. Walks through what it produces and the expected output.
4. **Running the network** — `./testnet/local-devnet/run.sh` and `./testnet/local-devnet/stop.sh`. Log paths, socket paths, how to point `cardano-cli query tip` at each.
5. **Running the soak** — `./testnet/local-devnet/soak.sh [DURATION_SECONDS]` (default 1800). What the live banner looks like and where evidence lands.
6. **Verifying results** — anatomy of `evidence/<ts>/report.md`, what each pass/fail predicate means, what a typical successful run looks like (block counts, parity %, included txs).
7. **Topology & port reference** — table of process / port / socket / config file / topology file.
8. **Configuration reference** — explains every override in `shelley-spec.json` and `conway-spec.json`, links to cardano-ledger docs for the rest.
9. **Troubleshooting** — five most likely failures (socket race on cold start, port clash with running soak, genesis-time-too-old, KES validity, macOS App Nap), each with the precise symptom and fix.
10. **What this validates / what it doesn't** — explicit scope: validates dugite forge + relay diffusion + N2C tx flow against Haskell ground-truth. Does NOT validate Byron, hard-fork combinator transitions, ledger replay from snapshot, governance enactment, or Plutus phase-2 (different testbeds).

### 5. Issue tracking

One GitHub issue with detailed acceptance criteria. Body includes:

- Problem statement and topology diagram (mirrors this spec).
- Acceptance checklist — one box per concrete artefact: scripts, templates, doc page, SUMMARY.md update, gitignore, first successful soak with attached `report.md`.
- Out-of-scope items (Byron, HFC, etc.) explicitly listed so reviewers don't ask for them.
- Labels: `documentation`, `testing`, `validation`.
- Linked from this design spec and from the doc page's footer.

### 6. Worktree and branching

Implementation happens in an isolated git worktree. Branch name: `feature/local-testnet-docs`. The `using-git-worktrees` skill drives worktree creation when implementation begins. The final PR rolls up:

- `testnet/local-devnet/` (scripts + templates)
- `docs/src/running/local-testnet.md`
- `docs/src/SUMMARY.md` (one-line edit)
- `.gitignore` additions
- First successful soak's `report.md` linked in the PR body as evidence

### 7. Local verification gate before opening the PR

Before opening the PR I will, inside the worktree:

1. Run `./testnet/local-devnet/setup.sh` from a clean state.
2. Run `./testnet/local-devnet/run.sh`; confirm all three sockets come up and tips advance.
3. Run `./testnet/local-devnet/soak.sh 1800` (the full 30 min).
4. Run `mdbook build docs/` to confirm the new page renders.
5. Attach the resulting `report.md` and key CSVs to the tracking issue and the PR.

If any of the four predicates in Section 3 fails on the first soak, root-cause it (likely a script bug or a config drift), fix, and re-run. The PR is not opened until we have a green soak.

## Risks

- **`cardano-cli conway genesis create-testnet-data` semantics drift across cardano-node versions.** Mitigation: pin to cardano-node 11.0.1 / cardano-cli 11.0.0 in the doc page's prerequisites; add a `setup.sh` precheck that refuses to run on older versions.
- **dugite-node may have undiscovered bugs specific to magic=42 networks.** This is itself a benefit of the testbed — finding such bugs is part of why we're building it — but the first soak may surface them. If so, fix dugite, re-run, attach a clean soak's report to the PR.
- **Genesis stake split (4 delegators across 2 pools at 50/50) may give an unlucky leader-slot distribution.** Mitigation: the "≥3 blocks per pool" threshold is well below expected and would only fail on a real wiring bug, not an unlucky slot lottery. If the test does flake on lottery variance after multiple runs, we'll raise epoch count or stake skew rather than lower the threshold.
- **macOS-specific issues** (App Nap, socket buffer sizes, fork timing) — wrapped under `caffeinate`; documented in troubleshooting; the testbed remains a macOS-first deliverable for now.

## Open questions

None at design time. Implementation will surface concrete script-level decisions (e.g., exact log-tail regexes, how to extract `issuer_vkey` from a dugite block log line) but those are details, not design unknowns.

## Implementation order

Implementation plan (the `writing-plans` skill output) will sequence:

1. Worktree creation + skeleton dirs + `.gitignore`.
2. `config/spec/` files (the two genesis overrides — committed before any script logic so spec correctness can be reviewed independently).
3. `setup.sh` (genesis + key generation + config rendering).
4. `run.sh` + `stop.sh` (process lifecycle).
5. `submit-txs.sh` (tx helper).
6. `soak.sh` + `verify.sh` (samplers + report).
7. Doc page + `SUMMARY.md` edit.
8. First soak run + evidence collection.
9. GitHub issue creation.
10. PR open with evidence attached.

Each step has a clear pass condition (sockets up, tips advancing, predicates green) before moving on.
