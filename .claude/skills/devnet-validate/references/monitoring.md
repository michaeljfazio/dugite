# Monitoring — logs, metrics, and what healthy looks like

## Process output and log files

Every run leaves three logs under `testnet/local-devnet/logs/`:
- `dugite-bp.log`   — the forger
- `dugite-relay.log` — the middle hop
- `cardano-bp.log`  — the Haskell validator (named cardano-bp historically)

PID files under `testnet/local-devnet/state/*.pid`.

## Prometheus endpoints

| Node | Port | Notes |
|---|---|---|
| `dugite-bp`    | 12798 | Default endpoint (dugite-monitor expects this) |
| `dugite-relay` | 12799 | Bumped to avoid listener collision |
| `cardano-bp`   | 12800 | EKG-backed Haskell metrics |

Scrape with `curl -s localhost:PORT/metrics`.

## Key dugite metrics

| Metric | Healthy | Sick |
|---|---|---|
| `dugite_tip_age_seconds` | <5 (steady-state); <60 immediately after boot | Climbing monotonically → stall (Issue #508 class) |
| `dugite_chain_density` | ≈ `activeSlotsCoeff` ± 20% (0.4-0.6 with f=0.5) | <0.3 → forge starvation; >0.7 → over-density (impossible without bug) |
| `dugite_forged_blocks_total` | Increments roughly every `1/f` slots | Flat for >2 min → forger broken |
| `dugite_adopted_blocks_total` | Increments on every block from the relay | Flat while peer's tip advances → chain selection bug |
| `dugite_peers_connected` | ≥ 1 throughout | 0 for >5s → network thrash |
| `dugite_mempool_txs` | Bumps when tx-zoo runs | Never decreases after block forge → mempool not draining |
| `dugite_chain_sync_intersect_state` | Steady at "real" point | Reverts to "origin" → stale intersection bug |

## Key cardano-node trace patterns (cardano-bp.log)

Cardano-node emits structured JSON traces by default. Grep these:

| Pattern | What it means |
|---|---|
| `TraceAdoptedBlock` | The Haskell ledger has applied a block — **proves dugite's block was accepted** |
| `TraceForgedInvalidBlock` | **CRITICAL FAILURE**: dugite forged a block Haskell rejected |
| `TraceMempoolAccepted` | A tx submitted via cardano-cli passed Haskell's Phase-1 validation |
| `TraceMempoolRejectedTx` | A tx was rejected — pair with `AddedTx`/`RemoveTxs` (memory: `reference_cardano_node_mempool_traces`) |
| `ChainSync ... mismatched` | Header/body mismatch — usually a CBOR encoding bug |
| `KESKeyExpiryEvent` | KES rollover (we don't expect this in <20min runs) |
| `BlockFetchClient ... timeout` | Body delivery stalled — relay or BP unresponsive |

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

Run these in a separate shell during the soak window.

```bash
# Metric snapshot (one-shot)
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
  | grep --line-buffered -E 'TraceAdoptedBlock|TraceForgedInvalidBlock|MempoolAccepted|Error'
```

## Boot-timing observations

A healthy boot:
- `dugite-relay` ready (socket present, port listening): <3s after launch
- `cardano-bp` ready: <8s after launch (Haskell node initialisation is the slow path)
- `dugite-bp` ready and chain advancing past slot 0: <5s after launch
- First forged block appears in `dugite-bp.log` within ~`1/f` slots (≈ 2s with f=0.5)

If any of these exceed 2× the expected time, log it as an anomaly. Boot regressions are easy to introduce and easy to ignore.
