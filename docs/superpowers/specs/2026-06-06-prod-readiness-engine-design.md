# Production-Readiness Engine — Design

**Date:** 2026-06-06
**Status:** Approved (design); pending implementation plan
**Author:** Ralph loop (brainstormed with Michael Fazio)

## Purpose

A continuous, autonomous engine that iteratively drives dugite-node to
production readiness by running a perpetual cycle of **analysis → research →
iteration → verification** against the preprod and mainnet networks. The
engine's north star is byte-exact conformance with cardano-node (Haskell):
zero ledger divergence, zero sync wedges, zero phase-2 disagreement, no
performance or robustness gaps.

It is a *brain*, not a new harness — it orchestrates the proven tooling that
already exists in this repo (the `haskell-ledger-cross-validation` and
`devnet-validate` skills, the `scripts/dev/*-watch.sh` family, the
`phase2_repro` example, APFS-clone replays, the Koios MCP, and the
`cardano-*-oracle` agents).

## Execution model

A **perpetual self-pacing loop** driven by `ScheduleWakeup` (dynamic mode).
Each wake runs exactly **one iteration** then reschedules. The heavy parallel
work of an iteration is delegated to a scoped `Workflow` (the *muscle*).
Long-running node processes (syncs, replays, soaks) run **out-of-band** as
background processes; the loop observes and steers them but never blocks a
turn waiting on them.

```
ScheduleWakeup (dynamic, perpetual)
  └─ each wake = ONE iteration:
       1. ASSESS     read engine-state.md + sample node health + gate status
       2. SCHEDULE   pick highest-impact gap whose harness FITS resources
       3. DRIVE      spawn Workflow(muscle): research → reproduce → fix → gauntlet
       4. RECORD     rewrite engine-state.md
       5. RESCHEDULE ScheduleWakeup(next), cadence tuned to what it waits on
  └─ terminates only when all 4 gates green; else perpetual (stop anytime)
```

## The five iteration phases

### 1. ASSESS
- Read `engine-state.md` (durable backlog + cross-wake memory).
- Sample live node(s): tip, stall-watch, `chain_diverged`, `ledger_tip` vs
  `immutable_tip`, blk/s, process RSS.
- Pull fresh gate status: latest epoch-diff dump, last conformance run, open
  `prod-readiness`/`epoch-diff` GitHub issues.
- Detect *new* divergences (a sync halt / `WithdrawalAmountMismatch` becomes a
  new backlog item).
- **Never SIGKILL** — SIGTERM only. `pkill -9` corrupts the append-only
  ImmutableDB and causes permanent deadlock.

### 2. SCHEDULE (resource- AND impact-aware)
- Rank open gaps by impact.
- Filter by *fit*: a gap needing a from-genesis replay cannot be picked while
  a live node holds the RAM (live preprod node ≈ 7.5 GB RSS leaves ≈ 2.5 GB
  free — heavy replay cannot run concurrently). The scheduler either:
  - (a) picks an **analysis-only** item that fits current resources, or
  - (b) if the top item needs the resource and is worth it, cleanly
    **SIGTERMs** the live node (writes a clean v21 snapshot → fast restart
    later) to free RAM, exactly as `POST-HOLD-PLAN.md` prescribes.
- Exactly **one item per iteration**.

### 3. DRIVE
Spawn a scoped `Workflow` for the picked item. Internal shape varies by gap
class (see Playbooks) but always:
- **Research** — `cardano-*-oracle` for canonical `IntersectMBO/cardano-ledger`
  Haskell source + ledger-spec citation. **Read the in-project
  `.claude/skills/haskell-ledger-cross-validation/references/era-rules/*.md`
  files FIRST** (they cover most ledger-rule questions with verbatim Haskell +
  permalinks before any live oracle query).
- **Reproduce** — APFS-clone the relevant db (`cp -Rc`, copy-on-write so the
  live sync db is untouched), targeted replay/dump with instrumentation, diff
  vs Koios / `cardano-cli debug log-epoch-state`.
- **Fix** — in a git worktree for isolation (`worktree` isolation in the
  Workflow, or the `validate-runner` worktree convention).
- **Gauntlet** — Section "Verification gauntlet". Commit + push **iff** all
  checks pass.

### 4. RECORD
Rewrite `engine-state.md`: move the item to done/blocked/in-progress, bump
attempt count, capture root-cause + evidence links, update last-known
node/replay state and which db-clones currently exist on disk.

### 5. RESCHEDULE
Pick the next wake delay from what it waits on:
- ~270s (cache-warm) while actively polling a replay it just launched.
- 1200–1800s when a multi-hour from-genesis sync is the gate.
- short/immediate when a fitting analysis item is queued.

## Verification gauntlet (the autonomy gate)

There is **no human checkpoint**. The gauntlet is the only thing between a
wrong fix and a poisoned commit. Tiered by risk class. Every outcome — pass or
fail, including refutation transcripts — is written to `engine-state.md` so a
later iteration never silently repeats a refuted approach.

### Tier A — reward / snapshot / era-transition / governance / fee math (#438 class)
Must clear **all four**, in order, or the fix is rejected and the item returns
to the backlog with the failure recorded:
1. **Oracle Haskell-source exact match** — changed logic quotes the canonical
   cardano-ledger source and matches it.
2. **Spec citation** — the relevant ledger-spec rule is quoted and consistent.
3. **Byte-exact replay reproduces reference** — a real replay over the affected
   window reproduces Koios / `cardano-cli debug log-epoch-state` with the
   divergence gone. **Tests passing is explicitly NOT sufficient** (the #438
   lesson: tests get rewritten to match wrong behavior).
4. **Adversarial refutation panel** — N independent skeptic agents each try to
   *refute* the fix from a distinct lens (Haskell-semantics, edge-epoch,
   compounding-feedback, integer-rounding). Majority-refute → rejected.

### Tier B — non-ledger (perf, network, CLI-format, decoder, tests, docs)
`fmt + clippy --all-targets -D warnings + nextest` all green + one focused
verifier agent + the relevant devnet/soak harness shows no regression.

## Dimension playbooks

Each readiness gate maps to existing assets; the engine orchestrates, it does
not reinvent.

| Gate | Playbook (existing asset) |
|---|---|
| **Ledger byte-exactness** | `haskell-ledger-cross-validation` skill + APFS-clone replay + Koios MCP + epoch-diff dumps |
| **Live sync to tip** | `scripts/dev/{sync-to-tip,stall,passive-sync}-watch.sh` + `soak-sample` + `Monitor` until-loops |
| **Phase-2 / UPLC** | `phase2_repro` example + fresh dump capture + ScriptContext field-diff vs Haskell |
| **Perf & robustness** | `devnet-validate` skill + `prof-run`/`soak-sample` + security tooling |

### Node-management rules (encoded in the loop)
- SIGTERM-only stops (never `pkill -9`).
- Long replays/syncs run via background Bash (`run_in_background`).
- Validation runs use the isolated `validate-runner` worktree convention.
- Multi-node runs get distinct N2N / N2C socket / metrics ports.
- Check binary mtime vs fix-commit time before declaring anything fixed
  (stale-binary rule); rebuild before re-test.
- Health-check cadence ≤ 10 min during active node runs.

## Readiness gates (definition of done)

All four must be green, with evidence in `engine-state.md`, for the engine to
declare production-ready:

1. **Ledger byte-exactness** — per-epoch reward/treasury/reserves/fees/
   deposits/snapshots/pool-state/governance byte-exact vs Haskell + Koios
   across full preprod AND mainnet history; zero `WithdrawalAmountMismatch`
   halts.
2. **Live sync to tip (both nets)** — from-genesis full-validation sync to
   current tip on preprod AND mainnet, then a sustained at-tip soak with no
   stall / wedge / `chain_diverged`, `ledger_tip == immutable_tip`, correct
   fork handling.
3. **Phase-2 / UPLC exactness** — every on-chain redeemer (V1/V2/V3) evaluates
   with byte-exact ExBudget + `is_valid` agreement vs Haskell across all
   captured mainnet/preprod redeemers.
4. **Performance & robustness** — acceptable sync throughput, bounded at-tip
   CPU/mem, security-audit findings closed, adversarial inputs rejected (not
   crashed), clean SIGTERM snapshot/restart.

Until all four are green the loop runs perpetually; it can be stopped at any
wake.

## Persistent artifacts the engine owns

1. **`engine-state.md`** — durable backlog + cross-wake memory. Single source
   of truth. Sections: ranked backlog (impact-tagged), in-progress item
   (attempts, blocked-on), gate-status board, last node/replay state, db-clones
   on disk, gauntlet ledger (passed/refuted approaches). Seeded once from
   existing docs/issues (`POST-HOLD-PLAN.md`, `REWARD-DIVERGENCE-*.md`,
   `MEMORY.md`, open GitHub issues), then self-maintained.
2. **Engine playbook** — the per-wake instruction set the loop re-reads each
   iteration (this design realized as an executable runbook).
3. **Running node/replay processes** — managed out-of-band per the
   node-management rules.

## Out of scope (YAGNI)

- No new validation harness — reuse existing skills/scripts.
- No human-approval UI — the gauntlet is the gate.
- No multi-repo / non-dugite targets.
- No re-implementation of epoch-diff dump tooling — it exists.
