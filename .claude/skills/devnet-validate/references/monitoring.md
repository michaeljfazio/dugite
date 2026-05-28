# Monitoring — logs, metrics, and what healthy looks like

> For a phase-by-phase model of node health (boot → catch-up → at-tip → forging → epoch boundary → restart), and a structured runtime evaluation procedure, see `health.md`. This file is the metric/log **reference catalog**; `health.md` is the **decision procedure**.

## Process output and log files

Every run leaves three logs under `testnet/local-devnet/logs/`:
- `dugite-bp.log`   — the forger
- `dugite-relay.log` — the middle hop
- `cardano-bp.log`  — the Haskell validator (named `cardano-bp` historically; it's actually the cardano-relay role with no forging keys)

PID files under `testnet/local-devnet/state/*.pid`.

## Prometheus endpoints

| Node | Port | Notes |
|---|---|---|
| `dugite-bp`    | 12798 | Default endpoint (dugite-monitor expects this) |
| `dugite-relay` | 12799 | Bumped to avoid listener collision |
| `cardano-bp`   | 12800 | EKG-backed Haskell metrics |

Scrape with `curl -s localhost:PORT/metrics`.

## Key dugite metrics — actual names (verified against `crates/dugite-node/src/metrics.rs`)

These are the metric names actually emitted by dugite-node today. The skill's earlier draft referred to several names that never existed (`dugite_chain_density`, `dugite_forged_blocks_total`, `dugite_adopted_blocks_total`, `dugite_mempool_txs`, `dugite_chain_sync_intersect_state`). Use the tables below as the authoritative list.

### Liveness / chain progress

| Metric | Type | Healthy | Sick |
|---|---|---|---|
| `dugite_tip_age_seconds` | gauge | <5 (steady-state); <60 immediately after boot | Climbing monotonically → stall (Issue #508 class) |
| `dugite_chainsync_idle_seconds` | gauge | <2 × `1/f` (so <4s with f=0.5) | >30s → ChainSync stuck or peer dead |
| `dugite_slot_number` | gauge | Strictly increasing every wall-clock second | Stuck → BP frozen (e.g. App Nap, deadlock) |
| `dugite_block_number` | gauge | Increases on every adopted/forged block | Flat while `slot_number` advances → forge starvation OR chain-selection bug |
| `dugite_epoch_number` | gauge | Increments at each boundary (every `epochLength` slots) | Wrong value → era/genesis miscalibration |
| `dugite_sync_progress_percent` | gauge | 10000 (=100%) once caught up | Stuck below 10000 with `max_peer_tip_slot` rising → catch-up bug |
| `dugite_max_peer_tip_slot` | gauge | Tracks the freshest peer tip | Stale → not receiving headers |

### Forging (BP only — only meaningful when `dugite_is_block_producer == 1`)

| Metric | Type | Healthy on devnet (f=0.5, σ=1.0) | Sick |
|---|---|---|---|
| `dugite_blocks_forged_total` | counter | Increments roughly every `1/f` slots (~every 2s with f=0.5) | Flat for >2 min on devnet → forger broken |
| `dugite_leader_checks_total` | counter | Exactly one increment per slot | Skipping slots → scheduler lag |
| `dugite_leader_checks_not_elected_total` | counter | `total − not_elected ≈ forged + race_lost + slot_battles` | Diff blows up → leader-check or forge-emit bug |
| `dugite_forge_failures_total` | counter | 0 in steady state | Any increment → quote `dugite-bp.log` reason verbatim |
| `dugite_forge_race_lost_total` | counter | 0 in single-forger devnet; non-zero on public testnets | High on devnet → forge-pipeline lag |
| `dugite_forge_slot_battles_total` | counter | 0 in steady state | Increments → wall-clock slot equals ledger tip slot (deadline-miss class) |
| `dugite_forge_announce_no_subscribers_total` | counter | 0 once a relay is connected | Increments → block diffusion broken (peer didn't subscribe to header announcements) |
| `dugite_blocks_announced_total` | counter | Tracks `blocks_forged_total` | Lag → diffusion broken |

### Diffusion / chain ingest

| Metric | Type | Healthy | Sick |
|---|---|---|---|
| `dugite_blocks_received_total` | counter | Increments on every block the peer sent | Flat while peer's tip advances → ChainSync stall |
| `dugite_blocks_applied_total` | counter | Tracks `blocks_received_total` minus small in-flight gap | Lag → ledger-apply pipeline blocked |
| `dugite_block_apply_failures_total` | counter | 0 | Any increment → ledger rejected a fetched block (Issue #669 class) |
| `dugite_rollback_count_total` | counter | Occasional bumps on competing forks | Storms of bumps → instability or peer-divergence |

### Mempool

| Metric | Type | Healthy | Sick |
|---|---|---|---|
| `dugite_mempool_tx_count` | gauge | Bumps when tx-zoo runs; drains within `1/f` per inclusion | Never decreases after a forge → mempool not draining |
| `dugite_mempool_bytes` | gauge | Bounded below `dugite_mempool_tx_max` × avg-tx-size | Saturated → mempool pressure |
| `dugite_mempool_tx_max` | gauge | Static config | — |
| `dugite_transactions_received_total` | counter | Increments on every N2C submit | — |
| `dugite_transactions_validated_total` | counter | Tracks `received_total` minus rejected | — |
| `dugite_transactions_rejected_total` | counter | Matches the deliberate negative tx-zoo count | Excess → false rejections |
| `dugite_n2c_txs_submitted_total` / `_accepted_total` / `_rejected_total` | counter | Mirror mempool counters | — |

### Peers and connections

| Metric | Type | Healthy | Sick |
|---|---|---|---|
| `dugite_peers_connected` | gauge | ≥1 throughout (devnet=1 in each direction) | 0 for >5s → network thrash |
| `dugite_peers_hot` | gauge | ≥1 (actively syncing) | 0 with `connected` ≥1 → peer stuck in warm state |
| `dugite_peers_warm` / `_cold` | gauge | Devnet: warm ≤ 1, cold = 0 | Bouncing → connection thrash |
| `dugite_peers_inbound` / `_outbound` / `_duplex` | gauge | Match topology expectations | — |
| `dugite_conn_full_duplex` | gauge | ≥1 between dugite-bp ↔ dugite-relay | 0 → simultaneous-open or mux-table bug |
| `dugite_conn_terminating` | gauge | 0 in steady state | Persistent non-zero → connection cleanup leak |
| `dugite_n2n_connections_active` | gauge | Equal to `peers_connected` after warmup | Drift → leaking conn-tracker entries |
| `dugite_n2n_connections_total` | counter | Increments only on intentional restart | Steady climb → connection thrash / peer flapping |
| `dugite_n2c_connections_active` | gauge | Bumps when tx-zoo runs cardano-cli; returns to 0 | Stuck >0 → N2C teardown bug |

### Ledger / state

| Metric | Type | Healthy | Sick |
|---|---|---|---|
| `dugite_utxo_count` | gauge | Monotonic-ish (creates ≫ consumes on devnet with forging) | Decreasing on devnet → UTxO accounting bug |
| `dugite_treasury_lovelace` / `dugite_reserves_lovelace` | gauge | Cross-check against per-epoch expectations | Drift vs Haskell dump → ledger-calc bug |
| `dugite_pool_count` / `dugite_delegation_count` / `dugite_drep_count` | gauge | Track genesis + cert activity | — |
| `dugite_ledger_replay_duration_seconds` | gauge | Only set on boot (one-shot) | Anomalously high → replay regression |

### Snapshot / I/O

| Metric | Type | Healthy | Sick |
|---|---|---|---|
| `dugite_snapshot_worker_alive` | gauge | 1 throughout | 0 → background snapshot worker died (Issue #695 class) |
| `dugite_snapshot_skipped_busy_total` | counter | A few in long runs is fine | Storm → snapshot worker can't keep up |
| `dugite_snapshot_failed_total` | counter | 0 | Any increment → I/O error or panic in worker |
| `dugite_utxo_flush_failed_total` | counter | 0 | Any increment → LSM flush failed |
| `dugite_disk_available_bytes` | gauge | > a few GB | Trending toward 0 → run out of disk before soak finishes |

### Config / identity

| Metric | Type | Meaning |
|---|---|---|
| `dugite_is_block_producer` | gauge | 1 if forge credentials are loaded; 0 otherwise |
| `dugite_network_magic` | gauge | 42 for the devnet; 2 preview; 1 preprod; 764824073 mainnet |
| `dugite_diffusion_mode` | gauge | 0 = InitiatorAndResponder (duplex), 1 = InitiatorOnly |
| `dugite_peer_sharing_enabled` | gauge | 0/1 |

### Cardano-node-shaped compat metrics

The node also emits `cardano_node_metrics_*` aliases (`slotNum_int`, `blockNum_int`, `epoch_int`, `connectedPeers_int`, `utxoSize_int`, `txsInMempool_int`, `mempoolBytes_int`, `Forge_forge_adopted_int`) so existing cardano-node Grafana dashboards "just work".

## Key cardano-node trace patterns (cardano-bp.log)

Cardano-node emits structured JSON traces by default. Grep these:

| Pattern | What it means |
|---|---|
| `TraceAdoptedBlock` | The Haskell ledger has applied a block — **proves dugite's block was accepted** |
| `TraceForgedInvalidBlock` | **CRITICAL FAILURE**: dugite forged a block Haskell rejected |
| `TraceDownloadedHeader` | A header arrived from the relay; should be followed by `TraceAdoptedBlock` for the same hash within a slot |
| `TraceMempoolAccepted` | A tx submitted via cardano-cli passed Haskell's Phase-1 validation |
| `TraceMempoolRejectedTx` | A tx was rejected — pair with `AddedTx`/`RemoveTxs` (memory: `reference_cardano_node_mempool_traces`) |
| `ChainSync ... mismatched` / `BlockFetch ... mismatched` | Header/body mismatch — usually a CBOR encoding bug |
| `KESKeyExpiryEvent` | KES rollover (we don't expect this in <20min runs) |
| `BlockFetchClient ... timeout` | Body delivery stalled — relay or BP unresponsive |
| `ConnectionLost` / `Disconnecting` | Peer churn; should be rare in a stable run |

## Key dugite log patterns (dugite-bp.log / dugite-relay.log)

| Pattern | Healthy? |
|---|---|
| `Forged block` / `forge slot=` | ✓ Forger is producing |
| `Adopted block` / `recv slot=` | ✓ Chain selection is advancing |
| `Switched to fork` | OK once or twice; persistent flip → instability |
| `Rejected tx` | OK only for negative tx-zoo tests |
| `ERROR` / `panicked` | ✗ Always a failure |
| `stale intersection` | ✗ See Round 3 / troubleshooting |
| `tip age` warnings | ✗ Issue #508 class — chain frozen |
| `KES sign failure` | ✗ Operational cert / KES key mismatch (memory: `project_opcert_signature_failure_2026_05_01`) |
| `forge_race_lost` / `forge_slot_battle` | △ A few are normal on busy networks; storms on devnet are a bug |

## Cross-validation oracle

`cardano-bp` is the **truth oracle** for block-level validation. Workflow:

1. dugite-bp forges block `B` at slot `S`.
2. dugite-relay receives and adopts `B`.
3. cardano-bp receives `B` from dugite-relay.
4. cardano-bp's Haskell ledger applies `B`.
   - SUCCESS → `TraceAdoptedBlock` in `cardano-bp.log`. **dugite-bp's block is byte-identical to what Haskell expects.**
   - FAILURE → `TraceForgedInvalidBlock` with a reason. **dugite has a ledger or serialization bug.**

If `cardano-bp` ever logs `TraceForgedInvalidBlock`, the round FAILS immediately. Capture:
- The block's slot, hash, era, body size
- The exact `cardano-bp` reason string
- The corresponding `dugite-bp.log` forge event
- Output of `cardano-cli query tip --socket-path state/cardano-bp.sock`

## Live sampling commands

Run these in a separate shell during the soak window. The bundled `scripts/health-probe.sh` wraps the most useful checks; prefer it for periodic sampling.

```bash
# One-shot health verdict (preferred — exits non-zero on anomaly)
.claude/skills/devnet-validate/scripts/health-probe.sh --verbose

# Metric snapshot (one-shot, all three endpoints)
for p in 12798 12799 12800; do
  echo "=== :$p ==="
  curl -s localhost:$p/metrics | grep -E '^dugite_|^cardano_'
done

# Continuous tip-age (issue #508 class)
while sleep 5; do
  printf '%(%T)T  ' -1
  for p in 12798 12799; do
    val=$(curl -s localhost:$p/metrics | awk '/^dugite_tip_age_seconds /{print $2}')
    printf ':%s=%s  ' "$p" "$val"
  done
  echo
done

# Forge/recv stream
tail -F testnet/local-devnet/logs/dugite-bp.log \
  | grep --line-buffered -E 'forge|recv|reject|ERROR|stale'

# Haskell adoption stream
tail -F testnet/local-devnet/logs/cardano-bp.log \
  | grep --line-buffered -E 'TraceAdoptedBlock|TraceForgedInvalidBlock|MempoolAccepted|mismatched|timeout|Error'
```

## Boot-timing observations

A healthy boot:
- `dugite-relay` ready (socket present, port listening): <3s after launch
- `cardano-bp` ready: <8s after launch (Haskell node initialisation is the slow path)
- `dugite-bp` ready and chain advancing past slot 0: <5s after launch
- First forged block appears in `dugite-bp.log` within ~`1/f` slots (≈ 2s with f=0.5)

If any of these exceed 2× the expected time, log it as an anomaly. Boot regressions are easy to introduce and easy to ignore.
