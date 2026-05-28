# Health — what a healthy dugite-node looks like and how to evaluate it at runtime

This file is the **decision procedure** the skill uses to evaluate whether a running dugite-node is healthy. It maps each node lifecycle phase to:
- the **expected steady-state signals** (metrics, log patterns, file system state)
- the **specific anomalies** that mark it as sick
- the **one-shot probes** that produce a verdict

For the metric/log catalog itself, see `monitoring.md`. For known failure modes and fixes, see `troubleshooting.md`.

## The six-question health model

A node is healthy at any instant iff all six questions answer **yes**:

1. **Liveness** — Is wall-clock advancing in the node? (`dugite_slot_number` strictly increasing)
2. **Connectivity** — Does it have at least one peer doing real work? (`dugite_peers_connected ≥ 1` AND `dugite_peers_hot ≥ 1` once past boot)
3. **Chain progress** — Is the tip moving relative to wall clock? (`dugite_tip_age_seconds` bounded; `dugite_block_number` advancing as blocks arrive)
4. **No regressions** — Are the "should always be zero" counters still zero? (`forge_failures`, `block_apply_failures`, `snapshot_failed`, `utxo_flush_failed`, plus zero `ERROR`/`panicked`/`TraceForgedInvalidBlock` lines)
5. **Network performance** — Are the mini-protocols carrying expected traffic? (block-reception rate, tx-reception rate, mux not idle for unhealthy intervals, no connection thrash)
6. **Cross-validation parity** — Does the co-running Haskell node agree on tip, accept every dugite-forged block, and stay within one block of dugite's tip?

Each question maps to one section of `scripts/health-probe.sh`. The probe exits 0 only when all six pass.

## Lifecycle phases and expected signatures

### Phase B — Boot (0–10s after launch)

Allowed-to-be-bad: tip age, sync progress, peer count.

A healthy boot leaves observable footprints in this order:

| Step | Signal | Expected by |
|---|---|---|
| Process started | PID file present, port LISTEN | <1s |
| Prometheus up | `curl :12798/metrics` returns text | <2s |
| Identity metrics emitted | `dugite_network_magic`, `dugite_is_block_producer`, `dugite_diffusion_mode` non-default | <2s |
| Ledger replay done | `dugite_ledger_replay_duration_seconds` set once | <5s on devnet, varies on testnets |
| First peer connected | `dugite_peers_connected ≥ 1` | <5s |
| First block adopted from peer (relay-side) | `dugite_blocks_applied_total ≥ 1` | <10s |
| Forger first leader check (BP only) | `dugite_leader_checks_total ≥ 1` | <2s of becoming BP |
| Forger first block emitted (BP only, devnet f=0.5) | `dugite_blocks_forged_total ≥ 1` | <10s after first leader check |

**Boot anomaly triggers:**
- Prometheus port not listening after 5s → process crashed silently; tail the log.
- `dugite_ledger_replay_duration_seconds` >2× last-known value at same chain height → replay regression.
- `dugite_peers_connected` stays 0 past 30s → topology / handshake bug (memory: `project_block_diffusion_failure`).
- BP launched but `dugite_is_block_producer == 0` → key flags missing/typo.
- `dugite_leader_checks_total` lags `dugite_slot_number` by >5 → forge scheduler not running.

### Phase C — Catch-up sync (booting into a non-empty chain)

The node is behind tip and pulling blocks as fast as it can.

| Metric | Healthy | Sick |
|---|---|---|
| `dugite_sync_progress_percent` | Strictly increasing, asymptotic to 10000 | Flat with `dugite_max_peer_tip_slot` rising → catch-up broken |
| `dugite_blocks_received_total` | Increasing rapidly (hundreds/sec on a hot path) | Stalls → ChainSync gap (memory: `project_chainsync_fragment_gap_fix_2026_05_10`) |
| `dugite_blocks_applied_total` | Tracks `received_total` minus in-flight | Diverges → ledger-apply pipeline blocked |
| `dugite_block_apply_failures_total` | 0 | Any increment → block fetched but ledger rejected it (Issue #669 class) |
| `dugite_tip_age_seconds` | Decreasing toward 0 | Plateauing well above 0 with peers caught up → stale intersection |

**Catch-up exit condition**: `dugite_sync_progress_percent == 10000` AND `dugite_tip_age_seconds < 5` AND the rate of `blocks_applied_total` has dropped to "one per slot-class interval" (chain pace).

### Phase T — At tip (steady state)

This is the most stringent phase — the one all soak runs target. All six health questions must hold continuously.

#### Universal steady-state signals (BP and relay)

| Signal | Healthy steady state |
|---|---|
| `dugite_tip_age_seconds` | Oscillates in `[0, 2/f]` (≈ `[0, 4]` on devnet) |
| `dugite_chainsync_idle_seconds` | Same band as `tip_age_seconds` |
| `dugite_slot_number` | Strictly increases every wall-clock second |
| `dugite_block_number` | Increases every `~1/f` slots on average |
| `dugite_peers_connected` | ≥1 throughout (devnet: 1; mainnet: target=20 default) |
| `dugite_peers_hot` | ≥1 (an actively-syncing peer) |
| `dugite_conn_terminating` | 0 |
| `dugite_snapshot_worker_alive` | 1 |
| `dugite_block_apply_failures_total` | Flat |
| `dugite_rollback_count_total` | At most a handful of bumps across an entire soak |
| ERROR/panic lines in any log | 0 |

#### BP-only steady-state signals (also requires `dugite_is_block_producer == 1`)

| Signal | Healthy steady state |
|---|---|
| `dugite_blocks_forged_total` | Increments at the σ-weighted Praos rate. On devnet (σ=1.0, f=0.5) ≈ every 2s. On preview (σ≈2.5e-5) ≈ 0.1 / epoch, so do NOT treat a flat counter as sick on public testnets (memory: `reference_praos_leader_probability`) |
| `dugite_leader_checks_total` | Increments exactly once per slot |
| `dugite_forge_failures_total` | 0 |
| `dugite_forge_slot_battles_total` | 0 on devnet |
| `dugite_forge_announce_no_subscribers_total` | 0 once a relay is subscribed |
| `dugite_blocks_announced_total` | Tracks `blocks_forged_total` |

**Steady-state evaluation cadence**: poll every ≤10 min during long soaks (memory: `feedback_node_health_check_cadence`). Each poll: tip + anomaly histogram + did-main-advance. The `health-probe.sh` script does this in one shot.

### Phase E — Epoch boundary

A healthy boundary crossing produces a brief, observable burst of activity, then returns to Phase T. The whole event should complete within a few slots.

| Signal | Expected at boundary |
|---|---|
| `dugite_epoch_number` | Increments by 1 |
| Log line | `epoch transition` / `EpochTransition` in `dugite-bp.log` |
| `dugite_treasury_lovelace` | Bumps (reward + deposit refunds + minted ada returned to treasury) |
| `dugite_reserves_lovelace` | Decreases (reward issue) |
| `dugite_pool_count`, `dugite_delegation_count`, `dugite_drep_count` | Snapshot-rotation deltas |
| `dugite_tip_age_seconds` | May spike to `[5, 15]` for ≤1 slot, then recover |
| `dugite_snapshot_skipped_busy_total` | May bump once (boundary triggers snapshot write) |
| `dugite_block_apply_failures_total` | Stays 0 |
| Haskell log `cardano-bp.log` | Multiple `TraceAdoptedBlock` post-boundary; no `TraceForgedInvalidBlock` |

**Boundary anomalies (any one is a fail):**
- `RUPD`, `pulser`, `reward calculation` errors in `dugite-bp.log`
- `dugite_treasury_lovelace` or `dugite_reserves_lovelace` diverge from Haskell dump (cross-check via `epoch-state-debug` feature or `cardano-cli debug log-epoch-state`)
- `analyze-evidence.sh` chain-density proxy (canonical-blocks ÷ slots) drifts outside `f × (1 ± 20%)`
- `TraceForgedInvalidBlock` for any post-boundary block — instant fail
- KES rollover during a <20-min run is unexpected; if `KESKeyExpiryEvent` appears, the genesis is misconfigured

### Phase R — Restart resilience

After SIGTERM/SIGKILL+relaunch, the node re-enters Phase B → Phase T quickly.

Health predicates (also Round 3 of the workflow):

| Predicate | Threshold |
|---|---|
| `dugite_tip_age_seconds` returns to <5 | within 60s |
| `dugite_block_number` advances past pre-restart value | within 60s |
| No `stale intersection` warning past the catch-up window | strict |
| `dugite_chainsync_idle_seconds` returns to band | within 60s |

If catch-up stalls past 60s, suspect the stale-intersection bug (memory: `project_stale_intersection_when_peer_behind`).

## Healthy ranges by lifecycle phase

| Metric | Boot | Catch-up | At-tip (devnet) | At-tip (preview, public) | Boundary |
|---|---|---|---|---|---|
| `dugite_tip_age_seconds` | n/a | ↓ to <5 | <5 | <60 | spike to <15 then <5 |
| `dugite_chainsync_idle_seconds` | n/a | <2 | <4 | <60 | <15 |
| `dugite_peers_connected` | →≥1 by 5s | ≥1 | ≥1 | ≥10 typical | ≥1 |
| `dugite_block_apply_failures_total` | 0 | 0 | 0 | 0 | 0 |
| `dugite_forge_failures_total` | 0 | n/a (relay) | 0 | 0 | 0 |
| `dugite_snapshot_worker_alive` | →1 | 1 | 1 | 1 | 1 |
| `dugite_rollback_count_total` (delta) | 0 | 0 | ≤2 / hour | ≤10 / hour | ≤1 |

## Don't-confuse-these red herrings

| Looks bad, isn't | Why |
|---|---|
| `dugite_blocks_forged_total` flat on **preview/preprod** for hours | σ is ~2.5e-5; P(0 forges per epoch) ≈ 90% (memory: `project_sandstone_pool_stake`, `reference_praos_leader_probability`). Don't trigger a forge-bug investigation until P(observed) < 1% AND the code path changed |
| `Switched to fork` once per epoch on public testnets | Normal — competing forks exist |
| `dugite_forge_race_lost_total` non-zero on multi-relay topologies | Inevitable on busy networks |
| `dugite_snapshot_skipped_busy_total` ticking up slowly | OK if `snapshot_failed_total` stays 0 and `snapshot_worker_alive == 1` |
| Tip-age spike at boundary | Snapshot pause; only an anomaly if it doesn't recover |
| `TraceMempoolRejectedTx` without a `RemoveTxs` companion | Benign retry pattern (memory: `reference_cardano_node_mempool_traces`) |

## Looks fine, isn't (silent failures to watch for)

| Looks OK, actually bad | Detection |
|---|---|
| `dugite_blocks_received_total` rising but `dugite_blocks_applied_total` flat | Ledger-apply stalled. Diff the two counters over a 30s window |
| `dugite_slot_number` advancing but `dugite_block_number` flat AND `peers_hot ≥ 1` | Chain selection broken or fork-stall (memory: `project_chainsel_fork_stall_bug`) |
| `dugite_peers_connected == 1` for hours on a multi-peer topology | Other peers silently rejected. Cross-check `dugite_n2n_connections_total` deltas |
| `dugite_treasury_lovelace` matches at the last poll but drifted mid-soak | Sample treasury/reserves at every boundary, not just at end |
| `dugite_is_block_producer == 1` but `dugite_leader_checks_total` flat | Forge scheduler died — must be caught by `health-probe.sh` |
| `TraceForgedInvalidBlock` only present in cardano-bp.log, not surfaced anywhere else | The probe greps `cardano-bp.log` and `verify.sh`/`analyze-evidence.sh` includes it in their exit-code gate |

## Network performance and behaviour

A node that is "up and connected" can still be silently sick at the network layer. These signals catch network-layer regressions that don't show up in tip/peer counts alone.

### Connection-quality signals

| Metric | Healthy steady state | Sick |
|---|---|---|
| `dugite_n2n_connections_active` | == `dugite_peers_connected` after warmup | Diverges → conn-tracker leak or zombie sockets |
| `dugite_conn_full_duplex` | ≥1 on the BP↔relay edge (loopback Duplex) | 0 with both processes alive → simultaneous-open bug (memory: `project_block_diffusion_failure`) |
| `dugite_conn_terminating` | 0 in steady state | Persistent ≥1 → connection teardown stuck |
| `dugite_n2n_connections_total` (delta) | Tiny — only on intentional restart | Climbing on its own → connection thrash (peer flapping) |
| `dugite_peers_warm` / `dugite_peers_cold` | Devnet: warm ≤1, cold = 0 | Oscillating → peer-governor instability |

### Mini-protocol throughput signals (delta-based)

Health is the **rate of change** of these counters between two probes (window = 30–60s). A counter going flat while wall clock advances is a fail.

| Counter | Devnet rate at-tip (f=0.5) | Sick |
|---|---|---|
| `dugite_blocks_received_total` (relay+BP combined) | ≥ 1 every `1/f` slots (~0.5/s) | Flat for >2 × `1/f` while peer's `block_number` rose → ChainSync stuck |
| `dugite_blocks_applied_total` | Tracks `received_total` minus ≤1 in-flight | Diverges → ledger-apply pipeline backlogged |
| `dugite_transactions_received_total` | Bumps during tx-zoo waves | Flat while tx-zoo is running → N2C → mempool path broken |
| `dugite_chainsync_idle_seconds` | <2 × `1/f` (so <4s) | >30 → ChainSync server has stopped pushing |
| `dugite_blocks_announced_total` | Tracks `blocks_forged_total` (BP) | Lags forge → block diffusion broken |

The probe samples these counters twice (separated by ≥5s) and computes the per-second rate. Define **net-stall** as: `dugite_slot_number` advanced ≥1 ticks AND `dugite_blocks_received_total` did not increase AND a hot peer is present. That's an instant FAIL on devnet; on public testnets it only fails if `chainsync_idle_seconds > 30` simultaneously.

### Connection thrash detector

Sample `dugite_n2n_connections_total` at probe N and N+1. If `delta(connections_total) > 2 * |delta(peers_connected)| + 1`, the node is dropping and re-establishing peers — symptom of handshake/version negotiation flakiness or peer-side resource exhaustion.

### Mempool drain rate (BP only)

At-tip steady-state, when a tx is submitted to the mempool, it should land in a forged block within `1 / f` seconds on average (devnet: 2s). The probe estimates drain rate by sampling `dugite_mempool_tx_count` + `dugite_mempool_bytes` over a 30s window:
- Steady non-zero mempool with `dugite_blocks_forged_total` advancing → drain working
- Steady non-zero mempool with forges advancing but mempool size NEVER dipping → drain broken (mempool→forge pipeline disconnect)

## Log sampling cadence

Static "ERROR / panic / WARN" counts at end-of-run miss the *temporal* shape of regressions (e.g. a burst of WARNs at one epoch boundary that subsides; a slow leak of `Switched to fork` once per minute). Use **delta-based, time-windowed log sampling**:

| Window | Cadence | What to grep | Anomaly trigger |
|---|---|---|---|
| Continuous | every probe (~60s) | `ERROR`, `panicked`, `stale intersection`, `KES sign failure` in all dugite logs | any **new** match since last probe |
| Continuous | every probe | `TraceForgedInvalidBlock` in `cardano-bp.log` | any match ever — instant fail |
| Per-minute bucket | every probe | `Switched to fork` count | >2 per minute |
| Per-minute bucket | every probe | `WARN` count | >2× rolling baseline median |
| Per-boundary | once per epoch transition observed | `RUPD`, `pulser`, `reward calculation`, `mismatch` in any log | any match |
| Per-boot | once | `Forged block`/`forge slot=` appears | absent after `1/f` slots → BP broken |

The probe writes the **baseline counts** to `${BASELINE_DIR}/errors-*` after each run, so the next invocation can compute deltas. First-call behaviour: establishes the baseline and treats deltas as 0.

### Sampling intervals

- Active soak runs (≤20 min total): probe every 60s; full log scan at end.
- Long-form test loops (Ralph): probe every ≤10 min (memory: `feedback_node_health_check_cadence`); never let a >10 min gap pass.
- Suspected stall: probe every 30s for the first 5 minutes, then back off.
- Boundary windows: tighten probe cadence to every 30s for 1 epoch around any expected transition.

## Cross-validation using the Haskell node

The co-running Haskell `cardano-bp` (the validator role, no forging keys) is more than a log oracle — it is a **live peer on the same network**. Use its participation to cross-check dugite continuously, not just at end-of-soak.

### Haskell-side metrics worth scraping

Cardano-node exposes EKG metrics on its Prometheus endpoint (default :12800 in the devnet topology). Scrape them every probe:

| Cardano-node metric | Healthy parity |
|---|---|
| `cardano_node_metrics_slotNum_int` | within ±5 slots of dugite's `dugite_slot_number` (loopback latency budget) |
| `cardano_node_metrics_blockNum_int` | within 1 of dugite's `dugite_block_number` (one block of in-flight diffusion) |
| `cardano_node_metrics_epoch_int` | equal to dugite's `dugite_epoch_number` |
| `cardano_node_metrics_connectedPeers_int` | ≥1 (dugite-relay) |
| `cardano_node_metrics_txsInMempool_int` | drains to 0 at the same forge rate dugite reports |
| `cardano_node_metrics_density_real` | within ±20% of `activeSlotsCoeff` |
| `cardano_node_metrics_Forge_*` | all zero — cardano-bp is configured as a relay |

If `cardano-bp` falls behind by >1 block AND stays there for >10s, dugite is producing blocks the Haskell relay isn't fetching — likely a block-fetch / body-diffusion bug.

### Haskell log patterns to watch in real time

Grep `cardano-bp.log` every probe and treat the **delta** as the signal:

| Pattern | Healthy expectation |
|---|---|
| `TraceAdoptedBlock` | One per dugite forge after a ≤1s diffusion lag. Delta over a 60s window should ≈ `f × 60` |
| `TraceForgedInvalidBlock` | 0 ever — instant fail with reason captured |
| `TraceMempoolAccepted` | One per tx-zoo submission |
| `TraceMempoolRejectedTx` | Only for deliberate negative tx-zoo cases; pair with `RemoveTxs` to distinguish benign retry (memory: `reference_cardano_node_mempool_traces`) |
| `ChainSync ... mismatched` / `BlockFetch ... mismatched` | 0 — encoding bug |
| `BlockFetchClient ... timeout` | 0 — body not arriving from dugite-relay |
| `TraceDownloadedHeader` | Tracks `TraceAdoptedBlock` minus 1–2; gap → header arrived but body never fetched |
| `Disconnecting` / `ConnectionLost` | Rare (peer restart only); delta should be 0 in a stable run |

### The "diffusion round-trip" cross-check

This is the most important cross-validation. For every block dugite forges, the round-trip should observe:

```
dugite-bp.log         : "Forged block ... slot=S hash=H"          (t=0)
dugite-relay.log      : "recv slot=S hash=H from dugite-bp"       (t≈+0.2s)
cardano-bp.log        : "TraceDownloadedHeader ... slot=S hash=H" (t≈+0.4s)
cardano-bp.log        : "TraceAdoptedBlock ... slot=S hash=H"     (t≈+0.5s)
```

Round-trip budget on a loopback devnet: ≤1s. Per-link latency observable from log timestamps. If the dugite-bp → cardano-bp gap exceeds 2s consistently, suspect the BlockFetch server in dugite-relay or the body-batching path.

`evidence/<ts>/blocks.csv` records the `recv`/`forge` events per observer with timestamps and hashes; `verify.sh p1` validates this round-trip at end-of-soak. The probe's job is to catch a degraded round-trip *during* the run so you can capture state before the symptom self-clears.

### Behavioural cross-validation beyond block adoption

| Behaviour | dugite expectation | Haskell expectation | Divergence = bug |
|---|---|---|---|
| Tx acceptance Phase-1 | `dugite_transactions_validated_total` increment, `Rejected tx` only for negatives | `TraceMempoolAccepted` for positives, `TraceMempoolRejectedTx` for negatives | Accept-set asymmetry (memory: `project_security_audit_integration_2026_05_19`) |
| Era / version | `dugite_epoch_number`, `dugite_pparam_*` reflect post-HF state | `cardano-cli query tip --socket-path state/cardano-bp.sock` reports same era + protocol version | Mismatch → era-transition bug (memory: `project_issue_481_pv9_hf_bump_fix`) |
| Protocol parameter change effects (Round 2) | `dugite_pparam_*` values update at boundary | `cardano-cli query protocol-parameters` against cardano-bp returns the same values | PPUP / governance ratification bug |
| UTxO total | `dugite_utxo_count` | `cardano-cli query utxo --whole-utxo` against cardano-bp gives same row count | UTxO accounting bug |
| Treasury / reserves at boundary | `dugite_treasury_lovelace`, `dugite_reserves_lovelace` | `cardano-cli query ledger-state --socket-path state/cardano-bp.sock \| jq` returns same values | RUPD / reward bug (memory: `project_issue_438_*`) |

The probe makes the cheap cross-checks (slotNum, blockNum, epoch, peers, mempool size) on every invocation. The expensive ones (UTxO total, treasury/reserves, full PParams) are explicit verify-step responsibilities, not per-probe.

## Effective runtime evaluation procedure

When asked "is the node healthy right now?", run this in order. Stop at the first FAIL — the rest are unreliable until it's resolved.

```
 1.  Process alive?              test -d /proc/$(cat state/dugite-bp.pid)   (Linux) / kill -0 $pid (macOS)
 2.  Prom port responding?       curl -fs --max-time 2 :12798/metrics > /dev/null
 3.  Wall-clock advancing?       two reads of dugite_slot_number 5s apart → strictly greater
 4.  Peer present?               dugite_peers_connected ≥ 1 AND dugite_peers_hot ≥ 1
 5.  At-tip?                     dugite_tip_age_seconds < 5  (relax to <60 on public testnets)
 6.  Apply pipeline clean?       dugite_block_apply_failures_total unchanged from baseline
 7.  Forge pipeline clean?       dugite_forge_failures_total unchanged AND (BP only) leader_checks_total advancing
 8.  Snapshot worker alive?      dugite_snapshot_worker_alive == 1
 9.  Network throughput OK?      delta(dugite_blocks_received_total) > 0 over 5s while slot advanced
10.  No connection thrash?       delta(dugite_n2n_connections_total) ≤ 2*|delta(peers_connected)| + 1
11.  Haskell-tip parity?         cardano-bp slotNum within ±5 of dugite slot AND blockNum within ±1
12.  Haskell adopted recent?     cardano-bp.log gained ≥1 TraceAdoptedBlock since last probe (BP only; devnet only)
13.  No new ERROR/panic?         grep -c 'ERROR\|panicked' logs/*.log unchanged from baseline
14.  Cross-validation clean?     grep -c 'TraceForgedInvalidBlock' logs/cardano-bp.log == 0
```

This is exactly what `scripts/health-probe.sh` implements. Use it; don't reinvent.

### When to run it

| Context | Cadence |
|---|---|
| Active soak ≤20 min | once at the end |
| Long-form test loop (Ralph) | ≤ every 10 min (memory: `feedback_node_health_check_cadence`) |
| Suspected stall | immediately, then every 60s until resolved |
| Pre-commit on changes touching node/ledger/network/forge | once after `./run.sh` + 30s warmup |

### Interpreting probe output

The probe prints **one line per check** plus a final verdict and exits non-zero on failure. Always quote its output verbatim — never paraphrase. The probe's anomaly list maps 1:1 to the issue classes documented in `troubleshooting.md`.

## Reasoning about a sick node

Diagnose, don't restart. Each phase has a default failure family:

| Phase | First-suspect family | Reference memory |
|---|---|---|
| Boot | port/topology/key flags | `project_block_diffusion_failure`, `project_bp_relay_pair_setup` |
| Catch-up | ChainSync fragment gap or stale intersection | `project_chainsync_fragment_gap_fix_2026_05_10`, `project_stale_intersection_when_peer_behind` |
| At-tip | chain-selection (Bug J class), App Nap | `project_bug_j_fixed_2026_05_16`, `project_macos_appnap_freeze_2026_05_08` |
| Boundary | RUPD / reward calc / PV-bump | `project_issue_438_*`, `project_issue_481_pv9_hf_bump_fix` |
| Restart | stale intersection | `project_stale_intersection_when_peer_behind` |
| Diffusion | mux-table / simultaneous open | `project_block_diffusion_failure` |

Always **capture before mitigating**: snapshot `evidence/<ts>/`, logs, and a final `health-probe.sh --verbose > probe.txt`. A restart that "fixes" the symptom without a captured snapshot destroys the root-cause evidence.
