# CSJ Phase F — Cross-validation against cardano-node

This directory contains the Phase F validation harness for Ouroboros Genesis
ChainSync Jumping (CSJ).  Phase F verifies that dugite's CSJ implementation
produces the same event sequence as cardano-node 10.6.x when both nodes sync
against the same mainnet (or preview/preprod) peer set in Genesis mode.

## Scripts

| Script | Purpose |
|--------|---------|
| `csj-phase-f-validate.sh` | dugite-node side — starts node in Genesis mode, captures CSJ trace events, writes summary |
| `csj-phase-f-haskell.sh` | cardano-node side — starts node in Genesis mode, captures JSON trace events, writes parallel summary |
| `test_csj_validation_smoke.sh` | 60-second smoke test — asserts Dynamo elected + zero LoE violations |

## How to invoke

### Quick smoke test (60 seconds, no 24h wait)

```bash
# Against preview (requires db-preview populated by mithril-import)
./scripts/validation/test_csj_validation_smoke.sh \
    --network-magic 2 \
    --config config/preview/config.json \
    --topology config/preview/topology.json \
    --database-path ./db-preview

# Against a specific peer (skips topology file):
./scripts/validation/test_csj_validation_smoke.sh \
    --network-magic 2 \
    --config config/preview/config.json \
    --topology config/preview/topology.json \
    --database-path ./db-preview \
    --peer-addr relays-new.cardano-testnet.iohkdev.io:3001

# With a debug binary:
./scripts/validation/test_csj_validation_smoke.sh \
    --dugite-bin ./target/debug/dugite-node \
    --network-magic 2 \
    --config config/preview/config.json \
    --topology config/preview/topology.json \
    --database-path ./db-preview
```

Exit code 0 = both assertions passed.  Exit code 1 = assertion failure.

### Full 24-hour mainnet run

Run both sides simultaneously on the same hardware (or on two hosts with
identical NTP-synchronized clocks):

**Terminal 1 — dugite:**
```bash
./scripts/validation/csj-phase-f-validate.sh \
    --network-magic 764824073 \
    --config config/mainnet/config.json \
    --topology config/mainnet/topology.json \
    --database-path ./db-mainnet \
    --duration 86400 \
    --out-dir validation/$(date +%Y%m%dT%H%M%SZ)
```

**Terminal 2 — cardano-node (Haskell):**
```bash
./scripts/validation/csj-phase-f-haskell.sh \
    --network-magic 764824073 \
    --config config/mainnet/config.json \
    --topology config/mainnet/topology.json \
    --database-path ./db-mainnet-haskell \
    --duration 86400 \
    --out-dir validation/$(date +%Y%m%dT%H%M%SZ)-haskell
```

Both nodes must use:
- the same network, topology, and genesis files
- `"ConsensusMode": "GenesisMode"` in their respective config files (or
  the equivalent `--consensus-mode genesis` CLI flag)
- fresh databases (or Mithril-imported databases from the same snapshot)

### Cardano-node Genesis mode configuration

Add or confirm the following fields in your cardano-node config JSON:

```json
{
  "ConsensusMode": "GenesisMode",
  "EnableP2P": true,
  "TraceChainSyncClient": true,
  "TraceChainSyncServerHeader": true
}
```

cardano-node >= 10.6.2 is required.  On macOS arm64 you must ad-hoc codesign
the binary (`codesign --sign - ./cardano-node`) and ensure its bundled dylibs
are present (see memory entry `project_preview_pv11_requires_cn11.md`).

## Expected output structure

```
validation/<timestamp>/
  dugite.log           Raw stderr of dugite-node
  csj_events.jsonl     One JSON object per CSJ event (see format below)
  loe_samples.jsonl    Prometheus LoE slot + connected peers every 30s
  summary.txt          Human-readable summary
  violations.txt       LoE violations — empty means pass

validation/<timestamp>-haskell/
  cardano-node.log
  haskell_events.jsonl     Normalised CSJ events
  haskell_raw_events.jsonl Raw JSON trace lines from cardano-node
  summary.txt
  violations.txt
```

### JSONL event format

Both scripts emit the same schema:

```json
{"ts":"2026-05-22T14:00:00Z","event":"DynamoElected","extra":{"peer":"1.2.3.4:3001"},"raw":"..."}
{"ts":"2026-05-22T14:00:05Z","event":"JumpIssued","extra":{"jump_slot":12345678},"raw":"..."}
{"ts":"2026-05-22T14:00:05Z","event":"IntersectFound","extra":{},"raw":"..."}
{"ts":"2026-05-22T14:00:10Z","event":"ObjectionRaised","extra":{},"raw":"..."}
{"ts":"2026-05-22T14:00:12Z","event":"ObjectionResolved","extra":{"outcome":"DynamoWins"},"raw":"..."}
```

Event vocabulary (same on both sides):

| Event | Haskell origin | Dugite origin |
|-------|---------------|---------------|
| `DynamoElected` | `TraceDynamoChanged` | `"CSJ: elected new dynamo"` |
| `DynamoStallDemotion` | `TraceDynamoTimedOut` | `"CSJ: dynamo stalled; demoting"` |
| `JumpIssued` | `TraceJumpResult` | `"CSJ: jump issued"` |
| `IntersectFound` | `TraceJumpResult{IntersectionFound}` | `"CSJ: intersect found"` |
| `ObjectionRaised` | `TraceObjectionRaised` | `"CSJ: intersect not found"` |
| `ObjectionResolved` | `TraceObjectionResolved` | `"CSJ/GDD: dynamo wins"` / `"CSJ/GDD: objector wins"` |
| `InvariantViolation` | _(no equivalent)_ | `"CSJ invariant violation"` |
| `LoEViolation` | `densityViolation` | Prometheus slot > loe_slot |

## Pass / fail criteria

### Automated (smoke test)

| Assertion | Criterion |
|-----------|-----------|
| (a) Dynamo activated | At least 1 `DynamoElected` event within `--duration` |
| (b) No LoE violations | `LoEViolation` event count = 0 |
| (c) No invariant violations | `InvariantViolation` event count = 0 |

### Operator (24h live run)

After the run, fill in `CSJ_PHASE_F_REPORT.template.md` and verify:

1. **Sync time delta < 10%**: dugite wall-clock sync time does not exceed
   cardano-node's sync time by more than 10% for the same number of blocks.

2. **Zero LoE violations**: `violations.txt` is empty on both sides.

3. **Trace event 1:1 correspondence**: The diff step (below) finds no
   unexplained divergences in event-type ordering within a 5-minute window.

4. **Dynamo election counts within 2x**: Both nodes should elect dynamos
   at roughly the same rate; a >2x ratio indicates a peer-selection skew.

## Trace-event equivalence and the diff step

"Trace-event equivalence" means that for every CSJ event in the Haskell
output within a given 5-minute window, there is a matching event (same type,
same approximate slot) in the dugite output, and vice versa.

Exact timestamp matching is not required because:
- The two nodes connect to different (though overlapping) peer sets.
- Peer latency differences shift when jumps are issued.
- GDD density comparisons depend on which blocks each node has received.

Perform the diff with:

```bash
# Extract just the event type sequence from each side.
jq -r '.event' validation/<timestamp>/csj_events.jsonl > /tmp/dugite_events.txt
jq -r '.event' validation/<timestamp>-haskell/haskell_events.jsonl > /tmp/haskell_events.txt

# Coarse diff: compare event-type distributions.
sort /tmp/dugite_events.txt | uniq -c | sort -rn > /tmp/dugite_dist.txt
sort /tmp/haskell_events.txt | uniq -c | sort -rn > /tmp/haskell_dist.txt
diff /tmp/dugite_dist.txt /tmp/haskell_dist.txt

# Fine diff: compare 5-minute bucketed event sequences.
# (Use the slot from extra.jump_slot or ts for bucketing.)
jq -r '[.ts[0:15], .event] | @tsv' validation/<timestamp>/csj_events.jsonl | sort > /tmp/d.tsv
jq -r '[.ts[0:15], .event] | @tsv' validation/<timestamp>-haskell/haskell_events.jsonl | sort > /tmp/h.tsv
diff /tmp/d.tsv /tmp/h.tsv
```

An acceptable diff has:
- No `LoEViolation` lines on either side.
- `DynamoElected` count within 2x of each other.
- `ObjectionRaised` and `ObjectionResolved` counts within 3x
  (GDD diverges when nodes have different peers).

## Why the 24h live run is not executed in-session

The full Phase F validation requires a synced mainnet database (~35 GB via
Mithril) on two hosts, 24+ hours of wall-clock time, and live inbound peers
from the Cardano P2P network.  These constraints cannot be satisfied inside
a CI session or an agent worktree.

The operator's checklist for the live run is documented in
`CSJ_PHASE_F_REPORT.template.md`.

## CSJ trace events in dugite source

The events captured by `csj-phase-f-validate.sh` map to these source locations:

- `DynamoElected` — `crates/dugite-node/src/csj_orchestrator.rs`, `elect_dynamo()`
- `DynamoStallDemotion` — `csj_orchestrator.rs`, `check_dynamo_stall()`
- `JumpIssued` — `csj_orchestrator.rs`, `handle_dynamo_tip_advanced()`
- `IntersectFound` — `csj_orchestrator.rs`, `handle_intersect_found()`
- `ObjectionRaised` — `csj_orchestrator.rs`, `handle_intersect_not_found()`
- `ObjectionResolved` — `csj_orchestrator.rs`, `handle_bisection_complete()`
- `InvariantViolation` — `csj_orchestrator.rs`, `assert_dynamo_invariant()`

The GSM events (`JumpAgreed`, `ObjectionRaised`, `ObjectionResolved`) are
forwarded to the GSM actor in `crates/dugite-node/src/gsm.rs` via the
`GsmEvent` enum and control the LoP (Limit on Patience) gate for the
`Syncing → CaughtUp` transition.

## Haskell reference

- `ouroboros-consensus-diffusion`: `ChainSync/Jumping.hs`
- Trace events: `TraceChainSyncClientEvent.TraceJumpResult`
- Genesis Governor: `Ouroboros/Consensus/Genesis/Governor.hs`
- Dynamo election: `csjSelectDynamo` (lowest-RTT hot peer)
- Demotion grace: `csjReprocessLoEDelay = 10 seconds`
