# scripts/soak/

Long-running validation rigs for the **preview Sandstone soak** — the public
preview testnet pool used to flush out forging, diffusion, mempool, and
chain-selection bugs over hours-to-days runs.

## Rigs

There are two supported topologies:

1. **BP-pair (default)** — dugite-node BP forging behind a co-located
   `cardano-node` Haskell relay. The Haskell relay handles all public peer
   diffusion; dugite only talks to the relay. This is the rig used to
   reproduce the soak findings tracked in the `project_*_soak_*` memories.
2. **Bare BP** — dugite-node BP talking directly to public peers (no local
   relay). Stresses dugite's own P2P stack; defaults to `BARE_BP=1` for the
   orchestrator.

## Scripts

| Script | What it does |
|--------|--------------|
| `run-6h.sh` | One-shot entry point: starts the BP-pair, kicks off the 6h orchestrator, prints status URLs. (`just soak-6h`) |
| `run-bare-bp.sh` | Same as above but uses `launch-bare-bp.sh` to start a bare BP, sets `BARE_BP=1` for the orchestrator. (`just soak-bare-bp`) |
| `orchestrator-6h.sh` | The 6-hour driver loop: every-30-min tx batch, every-5-min sync check, every-2-min health snapshot, every-1-min log scan, every-5-min process liveness with restart-on-death. |
| `status-6h.sh` | Snapshot of a running 6h soak (uptime, height/slot, recent forges, recent errors). (`just soak-status`) |
| `run-bp-pair.sh` | Boots a fresh Haskell relay + caffeinated dugite-node BP pair on preview. Wraps the BP in `caffeinate -dimsu` on macOS to dodge App-Nap freezes. |
| `launch-bare-bp.sh` | Boots a caffeinated bare dugite-node BP (no Haskell relay). |
| `varied-batch.sh` | Submits a mixed batch (simple, multi-out, metadata, 3-tx chain) every 30 min. Called by the orchestrator. |
| `restart-bp.sh` | Forced restart of the dugite BP (testing crash-recovery behavior). |

## Where output goes

- BP/relay process logs: `./logs/bp-pair/{bp,relay}-<timestamp>.log`
- Orchestrator + status output: `./logs/soak-6h/`
- PID files for cleanup: `./logs/bp-pair/{bp,relay}.pid`

All output paths are gitignored. Stale processes are detected and reaped on
each rig start.

## Pool

These rigs forge blocks for the **Sandstone** preview pool:

- Pool ID: `6954ec11cf7097a693721104139b96c54e7f3e2a8f9e7577630f7856`
- Active stake: see `project_sandstone_pool_stake` in agent memory

Cross-validation of forged blocks uses both the Haskell relay log
(`TraceDownloadedHeader`, `TraceAddedToCurrentChain`) and Koios
(`block_info`).
