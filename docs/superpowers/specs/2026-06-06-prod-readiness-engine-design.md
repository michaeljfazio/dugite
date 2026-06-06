# Production-Readiness Engine — Design

**Date:** 2026-06-06
**Status:** Approved (design); self-review revision applied; pending implementation plan
**Author:** Ralph loop (brainstormed with Michael Fazio)

> **Self-review revision (2026-06-06):** seven design flaws found and fixed —
> (1) the synchronous-Workflow fallacy (items are now multi-wake state machines;
> reproduce/verify are out-of-band, polled across wakes), (2) session-bound
> scheduling (added `CronCreate` durable substrate), (3) unbounded mainnet gate
> (replaced binary gates with an advancing-frontier model), (4) missing kill
> switch (halt sentinel), (5) no token-budget governor (added), (6) phase-2
> tiering gap (added Tier A′ for ScriptContext schema), (7) node-kill thrash +
> heavy-op contention (anti-thrash policy + single heavy-op lock). Plus
> Git/GitHub rules: all ops via `gh`, push over HTTPS, never SSH.
>
> **Autonomy hardening (2026-06-06):** added the *Autonomy invariants* section —
> 12 enumerated stall points and their no-human-intervention mitigations. The #1
> real-world risk is tool permission prompts (allowlist gaps: `cp -Rc`,
> `caffeinate`, several Koios MCP tools); bootstrap pre-extends the allowlist and
> the cron agent runs non-interactively. Also: no `AskUserQuestion`, no PR-merge
> wait (auto-merge), Koios-first reference data (no local node), disk GC, stale
> lock/worktree breakers, `caffeinate` wrap, idempotent crash recovery.

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

A **perpetual self-pacing loop**. Each wake runs exactly **one iteration** then
reschedules. The in-turn analysis/fix work of an iteration is delegated to a
scoped `Workflow` (the *muscle*). Long-running node processes (syncs, replays,
soaks) run **out-of-band** as background processes; the loop observes and steers
them across wakes but never blocks a turn waiting on them.

### Scheduler substrate: ScheduleWakeup vs Cron
- **`ScheduleWakeup` (dynamic mode)** — in-session. Simple, but **session-bound**:
  if the conversation ends, the loop stops. Use for an attended run.
- **`CronCreate` (remote scheduled agent)** — **durable across sessions/days/host
  restarts**. The engine's true "perpetual" form re-launches itself on a cron
  cadence, reading `engine-state.md` as its only memory. Recommended for an
  unattended multi-day drive. The two are interchangeable because **all
  cross-wake state lives in `engine-state.md`, never in conversation context** —
  a fresh cron invocation reconstructs full working state from that file.

### An "item" is a multi-wake state machine, NOT one synchronous pass
A backlog item's lifecycle spans **several wakes**, because reproduce/verify are
node runs that outlive any single turn. The loop advances each item through:
```
  NEW → REPRODUCING (launch replay bg, poll across wakes)
      → ANALYZING   (dump ready → Workflow muscle: research + root-cause)
      → FIXING      (Workflow muscle: patch in worktree)
      → VERIFYING   (launch verify-replay bg, poll across wakes)
      → GAUNTLET    (Workflow muscle: refutation panel over the verified result)
      → DONE | BLOCKED | REFUTED(→back to ANALYZING)
```
Only ANALYZING / FIXING / GAUNTLET run inside a synchronous Workflow. REPRODUCING
and VERIFYING are out-of-band background jobs the loop launches once and **polls**
on later wakes — it does not sit in a turn waiting for them.

```
each wake = ONE step of the active item's state machine:
  1. ASSESS     read engine-state.md + sample node health + gate status
                + CHECK HALT SENTINEL
  2. SCHEDULE   advance current item, OR pick highest-impact gap that FITS
                resources + token budget
  3. DRIVE      run the phase appropriate to the item's state (launch/poll a
                bg replay, OR spawn a Workflow for analysis/fix/gauntlet)
  4. RECORD     rewrite + COMMIT engine-state.md (git audit trail)
  5. RESCHEDULE next wake, cadence tuned to what it waits on
gate model: advance a continuously-verified FRONTIER (not binary green);
            run perpetually until all 4 frontiers reach tip; stop on sentinel
```

## The five iteration phases

### 1. ASSESS
- **Check the halt sentinel FIRST.** If `engine-state.md` carries `HALT: true`
  (or a `.engine-halt` file exists), finish recording and do **not** reschedule
  — clean stop between iterations, no mid-replay kill. This is the kill switch.
- Read `engine-state.md` (durable backlog + cross-wake memory).
- Sample live node(s): tip, stall-watch, `chain_diverged`, `ledger_tip` vs
  `immutable_tip`, blk/s, process RSS, free system RAM.
- Poll any out-of-band job launched on a prior wake (a REPRODUCING/VERIFYING
  replay): is it still running, finished, or wedged? Advance its item's state.
- Pull fresh gate status: latest epoch-diff dump, last conformance run, open
  `prod-readiness`/`epoch-diff` GitHub issues.
- Detect *new* divergences (a sync halt / `WithdrawalAmountMismatch` becomes a
  new backlog item).
- **Never SIGKILL** — SIGTERM only. `pkill -9` corrupts the append-only
  ImmutableDB and causes permanent deadlock.

### 2. SCHEDULE (resource-, impact-, AND budget-aware)
- If the current item is mid-state-machine, default to **advancing it** rather
  than starting a new one (avoid leaving replays/worktrees half-done).
- Otherwise rank open gaps by impact, then filter by **three** constraints:
  - **Token budget** — if the per-day budget is near-exhausted, prefer a
    cheap analysis/poll step or extend the wake cadence; never start a fresh
    fan-out Workflow when the budget can't cover it. (See Budget governor.)
  - **Heavy-op lock** — at most **one** heavy local operation (cargo build,
    replay, sync) runs at a time, guarded by a `.engine-heavyop.lock`. An item
    needing a heavy op that's already held waits; the loop picks a lock-free
    item instead.
  - **RAM fit** — a from-genesis replay cannot run while a live node holds the
    RAM (live preprod node ≈ 7.5 GB RSS leaves ≈ 2.5 GB free). The scheduler:
    - (a) picks an **analysis-only / lock-free** item that fits, or
    - (b) only if the top item strictly dominates, cleanly **SIGTERMs** the
      live node (clean v21 snapshot → fast restart) to free RAM.
- **Anti-thrash policy:** never SIGTERM a node that is *actively advancing a
  gate frontier* (sync climbing, soak accruing clean minutes) merely to free
  RAM for a lower- or equal-impact replay. Record the dominance decision +
  the frontier cost in `engine-state.md`; if a kill would regress a green-ish
  gate, defer the replay until the node next reaches a natural checkpoint.
- Exactly **one item advanced per iteration**.

### 3. DRIVE — run the phase appropriate to the item's STATE
The action depends on which state-machine state the item is in (not one fixed
synchronous pass):

- **REPRODUCING / VERIFYING (out-of-band, loop-owned):** acquire the heavy-op
  lock, APFS-clone the relevant db (`cp -Rc`, copy-on-write so the live sync db
  is untouched), launch the instrumented replay/dump as a **background** job,
  record its PID + log path, and return. On later wakes, **poll** it (ASSESS);
  when the dump is ready, diff vs Koios / `cardano-cli debug log-epoch-state`
  and advance the state. Release the lock when the job ends.
- **ANALYZING / FIXING / GAUNTLET (in-turn, Workflow muscle):** spawn a scoped
  `Workflow`. Shape varies by gap class (see Playbooks):
  - **Research** — `cardano-*-oracle` for canonical `IntersectMBO/cardano-ledger`
    Haskell source + ledger-spec citation. **Read the in-project
    `.claude/skills/haskell-ledger-cross-validation/references/era-rules/*.md`
    files FIRST** (verbatim Haskell + permalinks, before any live oracle query).
  - **Fix** — in a git worktree for isolation (`worktree` isolation in the
    Workflow, or the `validate-runner` worktree convention).
  - **Gauntlet** — runs over the *already-produced* verify-replay result from
    the VERIFYING state. Commit + push **iff** all gauntlet checks pass (push
    over HTTPS via `gh` auth — see Git/GitHub rules).

### 4. RECORD
Rewrite **and commit** `engine-state.md`: move the item to its new state, bump
attempt count, capture root-cause + evidence links, update last-known
node/replay state and which db-clones currently exist on disk, and append the
token spend for this wake. **Committing each wake** gives a git audit trail and
makes the state recoverable if a rewrite is interrupted — the file is the
engine's whole memory, so it is never left only in working-tree limbo.

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
4. **Adversarial refutation panel** — `N` independent skeptic agents each try
   to *refute* the fix from a distinct lens (Haskell-semantics, edge-epoch,
   compounding-feedback, integer-rounding). Majority-refute → rejected.
   Default `N = 3` (majority = 2); tunable in `engine-state.md`.

### Tier A′ — phase-2 / ScriptContext schema fixes
A wrong `ScriptContext` field can make a script pass `is_valid` *by luck*, so
phase-2 schema changes (`script_context.rs`, `populate_v1_v2.rs`, cost models)
get a Tier-A-strength gate, adapted:
1. **Oracle Haskell-source match** — the field encoding/order matches the
   canonical Plutus `ScriptContext`/`TxInfo` builder.
2. **Field-diff vs a Haskell ScriptContext dump** — not just `is_valid` parity:
   the reconstructed context matches Haskell field-by-field for the repro case.
3. **`phase2_repro` reproduces byte-exact ExBudget + `is_valid`** across the
   bucket's representatives (budget / Error / unIData classes).
4. **Refutation panel** as Tier A (lens set: schema-nesting, era-fallback,
   builtin-cost, data-encoding).

### Tier B — other non-ledger (perf, network, CLI-format, decoder, tests, docs)
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

### Git/GitHub rules (encoded in the loop)
- **All GitHub operations go through the `gh` CLI** — issues, PR creation/
  review, releases, status, API reads. Never raw GitHub web/API flows that need
  separate auth.
- **Push over HTTPS using `gh` credentials, never an SSH remote.** The engine
  is unattended, so an interactive SSH key authorization would wedge it. Ensure
  the `origin` remote is HTTPS and `gh auth` is the credential helper before the
  first push; if `origin` is SSH, the loop switches it to HTTPS (or pushes to an
  HTTPS-configured remote) rather than prompting.
- Focused commits only: stage explicit filenames (no `git add -A` / `-a`); keep
  each commit within ≤ 2 crates (`DUGITE_PRECOMMIT_STRICT=1` for the agent run).
- Work happens off `main` on a feature branch; integration via `gh pr` when a
  fix is gauntlet-green.

## Readiness gates — the FRONTIER model (definition of done)

Each gate is tracked as a **continuously-advancing verified frontier**, not a
binary flag. "Byte-exact across full mainnet history" is days of compute and
can regress as the chain grows, so the honest measure is *how far the verified
frontier has advanced, with zero open divergence behind it*. The engine's job
each iteration is to **advance a frontier or close a divergence behind one**.

Frontier per gate (recorded with evidence in `engine-state.md`):

1. **Ledger byte-exactness** — "byte-exact vs Haskell + Koios through epoch N"
   for preprod and for mainnet (two frontiers): per-epoch reward/treasury/
   reserves/fees/deposits/snapshots/pool-state/governance, zero open divergence
   and zero `WithdrawalAmountMismatch` halt behind the frontier.
2. **Live sync to tip (both nets)** — "from-genesis full-validation sync reached
   slot S, sustained at-tip soak of D hours clean" per net: no stall / wedge /
   `chain_diverged`, `ledger_tip == immutable_tip`, correct fork handling.
3. **Phase-2 / UPLC exactness** — "every captured redeemer through epoch N
   evaluates byte-exact ExBudget + `is_valid` vs Haskell" per net; open
   divergence buckets (budget / Error / unIData) tracked to zero.
4. **Performance & robustness** — sync blk/s ≥ target, at-tip CPU/mem within
   bound, security-audit findings closed, adversarial inputs rejected (not
   crashed), clean SIGTERM snapshot/restart verified.

**Production-ready** = all four frontiers reach current tip on both nets with
zero open divergence behind them and the perf/robustness checks green. Until
then the loop runs perpetually; it stops cleanly on the halt sentinel at any
wake.

## Budget governor

A perpetual Workflow-spawning loop has unbounded cost, so the engine is
budget-aware:
- A configurable **per-day output-token budget** in `engine-state.md`. Each
  wake records its spend (RECORD phase); ASSESS sums the rolling 24 h.
- When the day's budget is ≥ ~80 % spent, SCHEDULE prefers cheap poll/analysis
  steps and **extends the wake cadence** rather than launching fresh fan-out
  Workflows. At 100 % it only polls in-flight out-of-band jobs and otherwise
  idles at the long cadence until the window rolls.
- Workflow muscles scale their finder/refuter fan-out to remaining budget
  (`budget.remaining()` in the script), shrinking panels when tight.

## Autonomy invariants — ZERO human intervention to proceed

The engine must never block waiting on a human. The **only** intended human
touchpoint is the halt sentinel — and that is a *stop*, not a *proceed*, so it
can never stall progress. Every other potential stall point and its autonomous
mitigation:

1. **Tool permission prompts (the #1 real-world stall).** An allowlist gap
   blocks the loop. Mitigations, both required:
   - The engine's cron/loop agent runs under a **non-interactive permission
     posture** (no prompt can block it).
   - Bootstrap **pre-extends `.claude/settings.local.json`** to cover every
     command the engine issues. Known gaps today: `cp` (the `cp -Rc` APFS clone
     is core to every replay), `caffeinate`, `df`, `mv`, `mkdir`, `cat`,
     `date:*`, and the Koios MCP tools the ledger gauntlet needs
     (`koios_account_reward_history`, `koios_pool_history`,
     `koios_pool_delegators_history`, `koios_pool_stake_snapshot`,
     `koios_epoch_params`, `koios_tip`, …). The bootstrap step audits the
     runbook's command set against the allowlist and adds the difference.
2. **No interactive clarification.** The engine **never** calls
   `AskUserQuestion` or otherwise asks the user a question. Ambiguity resolves
   to a recorded default in `engine-state.md`, never a prompt.
3. **No push/auth prompt.** HTTPS remote + `gh` keyring token (verified). If a
   `gh` call ever fails auth, record the item `BLOCKED` and continue other work
   — never prompt.
4. **No PR-merge wait.** A human reviewer gate would stall the loop. Gauntlet-
   green commits push directly to the long-lived engine branch; integration to
   `main` (if used) goes via `gh pr merge --auto --squash` (auto-merges once CI
   is green) — never waits for a human review.
5. **Reference data without a local node.** Prefer **Koios (MCP)** for ground
   truth — always available, no local Haskell node required. Use
   `cardano-cli debug log-epoch-state` only when a reference node is *already*
   running; never stand one up interactively. (Confirmed: no node running now.)
6. **Disk exhaustion.** ASSESS samples free disk; the loop **GCs stale
   db-clones/dumps autonomously** (keep last N per net), and refuses to start a
   clone that won't fit rather than filling the disk — never waits for human
   cleanup.
7. **Stale heavy-op lock.** `.engine-heavyop.lock` carries holder PID + start
   time; if the PID is dead or the lock exceeds its TTL, the next wake reclaims
   it. A crashed wake can never permanently wedge the lock.
8. **Stale git/worktree locks.** Wake start detects and clears abandoned
   `.git/index.lock` and orphaned worktrees before any git op.
9. **macOS App Nap freeze.** All long-running node/replay processes are wrapped
   in `caffeinate -dimsu` (App Nap once froze a BP across a leader slot).
10. **Crash mid-wake.** State is committed every wake and transitions are
    idempotent, so the next wake reconstructs full working state from
    `engine-state.md` and resumes — no human recovery.
11. **Network/oracle/Koios flakiness.** Retry with backoff; on persistent
    failure mark the *item* `BLOCKED` and switch to a different item — a single
    flaky dependency never stalls the whole loop.
12. **Time source for budget/cadence windows.** Workflow scripts cannot call
    `Date.now()`; the loop turn reads wall-clock via Bash `date` and passes it
    into any Workflow via `args`.

## Persistent artifacts the engine owns

1. **`engine-state.md`** — durable backlog + cross-wake memory. Single source
   of truth; **committed every wake**. Sections: `HALT` flag + tunables
   (refuter `N`, daily token budget, cadence floor/ceiling), ranked backlog
   (impact-tagged), per-item state-machine status (attempts, blocked-on,
   current state), the four gate **frontiers**, last node/replay state +
   running-job PIDs/logs, db-clones on disk, heavy-op lock holder, rolling 24 h
   token spend, and the gauntlet ledger (passed/refuted approaches — so a
   refuted fix is never silently retried). Seeded once from existing
   docs/issues (`POST-HOLD-PLAN.md`, `REWARD-DIVERGENCE-*.md`, `MEMORY.md`,
   open GitHub issues), then self-maintained.
2. **Engine playbook** — the per-wake instruction set the loop re-reads each
   iteration (this design realized as an executable runbook the cron/loop
   invokes). Stateless w.r.t. context: everything it needs it reloads from
   `engine-state.md`.
3. **Halt sentinel** — `HALT: true` in `engine-state.md` (or a `.engine-halt`
   file). Checked first in ASSESS; the clean kill switch.
4. **Heavy-op lock** — `.engine-heavyop.lock` naming the single in-flight heavy
   local operation; enforces the one-heavy-op-at-a-time invariant.
5. **Running node/replay processes** — managed out-of-band per the
   node-management rules.

## Out of scope (YAGNI)

- No new validation harness — reuse existing skills/scripts.
- No human-approval UI — the gauntlet is the gate.
- No multi-repo / non-dugite targets.
- No re-implementation of epoch-diff dump tooling — it exists.
