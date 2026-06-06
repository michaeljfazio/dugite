# Production-Readiness Engine

An autonomous, perpetual engine that drives dugite-node to byte-exact
conformance with cardano-node on preprod and mainnet — with **zero human
intervention**. It runs a continuous cycle of **analysis → research → iteration
→ verification**, advancing four readiness frontiers (ledger byte-exactness,
live sync, phase-2/UPLC, performance/robustness) until all reach tip.

- Design spec: `docs/superpowers/specs/2026-06-06-prod-readiness-engine-design.md`
- Implementation plan: `docs/superpowers/plans/2026-06-06-prod-readiness-engine.md`

## How it works

Each **wake** runs one iteration of the five-phase loop in `engine-runbook.md`
(ASSESS → SCHEDULE → DRIVE → RECORD → RESCHEDULE) and advances exactly one
backlog item through a multi-wake state machine
(`NEW→REPRODUCING→ANALYZING→FIXING→VERIFYING→GAUNTLET→DONE`). The only cross-wake
memory is `engine-state.md`, committed every wake. Long node replays run
out-of-band (caffeinate-wrapped, background) and are polled across wakes. A fix
is committed only after it clears the verification gauntlet (Haskell-source
match + spec + byte-exact replay-reproduces-reference + adversarial refutation).

```
scripts/prod-readiness/
  engine-runbook.md      the per-wake algorithm (the brain)
  engine-state.md        durable single source of truth (committed every wake)
  muscle.workflow.js     Workflow muscle: research / root-cause / fix / gauntlet
  bootstrap.sh           one-time: preflight + allowlist audit/extend
  lib/                   deterministic helpers (lock, health, gc, clone, replay, poll)
  test/                  unit + smoke checks
```

## Prerequisites

Build the node with the epoch-state-debug feature so ledger replays emit dumps:

```bash
cargo build --release -p dugite-node --features dugite-ledger/epoch-state-debug
```

## Start

1. **Bootstrap once** (idempotent — preflight + extend the local allowlist):

   ```bash
   bash scripts/prod-readiness/bootstrap.sh
   ```

2. **Run the engine.** Two modes:

   - **Attended (in-session):** tell Claude *"Execute one wake of
     `scripts/prod-readiness/engine-runbook.md`"*. It runs one wake and calls
     `ScheduleWakeup` to continue itself. The loop lives as long as the session.

   - **Unattended (durable across sessions/days):** create a standing
     `CronCreate` whose prompt is *"Execute one wake of
     `scripts/prod-readiness/engine-runbook.md`"*. Because all state lives in
     `engine-state.md`, each cron invocation reconstructs full working state — no
     conversation context needed.

## Stop

Set the halt sentinel — the engine stops **cleanly at the next wake**, never
mid-replay:

```bash
touch scripts/prod-readiness/.engine-halt        # or set 'HALT: true' in engine-state.md
```

Resume by removing it (`rm scripts/prod-readiness/.engine-halt`) / setting
`HALT: false`.

## Monitor

```bash
# per-wake audit trail (one commit per wake)
git log --oneline -- scripts/prod-readiness/engine-state.md

# current node + host health
bash scripts/prod-readiness/lib/health-sample.sh

# in-flight replay logs
tail -f scripts/prod-readiness/.jobs/*.log

# current backlog / frontiers / in-progress item
sed -n '/## Frontiers/,/## Last node state/p' scripts/prod-readiness/engine-state.md
```

## Test

```bash
bash scripts/prod-readiness/test/test-heavyop-lock.sh
bash scripts/prod-readiness/test/test-gc-disk.sh
bash scripts/prod-readiness/test/test-smoke-wake.sh
for f in $(find scripts/prod-readiness -name '*.sh'); do shellcheck "$f"; done
```

## Autonomy guarantees

The only human touchpoint is the halt sentinel (a *stop*, never a *proceed*).
Every other potential stall is mitigated: non-interactive permissions +
allowlist (`bootstrap.sh`), no `AskUserQuestion`, HTTPS/`gh` push (no SSH),
auto-merge (no PR wait), Koios-first reference data (no local node required),
disk GC, dead-PID/TTL lock reclaim, stale git/worktree cleanup, `caffeinate`
wrap, and idempotent crash recovery. See the *Autonomy invariants* section of
the design spec.
