# Engine Runbook — one wake

You are executing **one wake** of the dugite production-readiness engine. Your
entire memory across wakes is `scripts/prod-readiness/engine-state.md`
(hereafter *STATE*). Everything you learn this wake that matters next wake MUST
be written there. Do exactly one wake — the five phases below — then schedule
the next and stop.

## Cardinal rules (autonomy — never break these)

- **Never call `AskUserQuestion`. Never ask the user anything.** On ambiguity,
  choose the most defensible default, record it in STATE under the item, proceed.
- **Never `pkill -9` / SIGKILL a dugite-node.** SIGTERM only (clean v21 snapshot).
  `pkill -9` corrupts the append-only ImmutableDB → permanent deadlock.
- **Tests passing is NOT evidence of byte-exactness.** Only a replay that
  reproduces Koios / a cardano-node dump with the divergence gone counts.
- **All GitHub ops via `gh`. Push over HTTPS** (never SSH). Focused commits,
  explicit filenames, ≤ 2 crates per commit, `DUGITE_PRECOMMIT_STRICT=1`.
- **Koios is the default ground truth** (no local node needed), reached via
  `scripts/prod-readiness/lib/koios.sh <net> <endpoint> '<json-body>'` (per-network
  REST). **Do NOT use the `koios_*` MCP tools** — they were observed serving the
  wrong network (Preview epoch 1320 when preprod ep293 was expected), which
  silently breaks byte-exact comparison. Use a `cardano-cli debug log-epoch-state`
  dump only when `reference_node_socket` in STATE is not `none`.
- Helpers live in `scripts/prod-readiness/lib/`. Prefer them over improvising
  shell. Run them from the repo root.

## Observability

Route **every analytical step** (diagnose / analyze / fix / gauntlet) through the
muscle Workflow so it appears live in `/workflows` while the engine is reasoning.
Keep **mechanical steps** (clone / launch / poll / commit) as direct shell — a
Workflow can only spawn subagents, so wrapping a trivial `poll-job.sh` in one
would waste a subagent. Net effect: `/workflows` shows the engine whenever it is
*thinking*; the per-wake `git log` of `engine-state.md` and the `.jobs/*.log`
files show it *waiting/working* on out-of-band replays.

---

## Phase 1 — ASSESS

0. **Acquire the wake-lock FIRST (before anything else).** Run
   `bash scripts/prod-readiness/lib/wake-lock.sh acquire`. If it prints `busy`,
   another wake is already mid-flight (can happen under cron if a long
   fix+nextest wake overran the period) — **STOP immediately: do nothing and do
   NOT reschedule.** The active wake owns the loop. Proceed only on `acquired`,
   and release it in Phase 5.
1. **Halt check FIRST.** Run `bash scripts/prod-readiness/lib/health-sample.sh`.
   If `"halt":true` (or `scripts/prod-readiness/.engine-halt` exists, or STATE
   `Control: HALT: true`): write a final RECORD note "halted by sentinel" and
   **STOP — do not reschedule.** This is the clean kill switch.
2. **Load STATE.** Read `engine-state.md` fully: Control, Frontiers, Backlog,
   In-progress, Running jobs, DB clones, Gauntlet ledger, Token spend, Last node state.
3. **Sample health** (from step 1's JSON): note `free_disk_gb`, `free_ram_gb`,
   `rss_mb`, `jobs_running`, `node_pids`. For a live node whose tip you want,
   re-run with `HS_SOCKET=<sock> HS_MAGIC=<n>` set.
4. **Poll out-of-band jobs.** For each entry under *Running jobs*, run
   `bash scripts/prod-readiness/lib/poll-job.sh <job>`:
   - `running` → leave it; this item stays in its current state.
   - `wedged` → SIGTERM its pid, mark the item `BLOCKED` with reason "replay
     wedged", release the heavy-op lock (`heavyop-lock.sh release`).
   - `done` → the replay finished; advance the item (REPRODUCING→ANALYZING or
     VERIFYING→GAUNTLET), release the lock, and (REPRODUCING) diff the dump.
5. **Refresh gate status.** If a dump just completed, diff it vs Koios (or the
   reference dump) to find/confirm the first open divergence per frontier. Pull
   open issues: `gh issue list --label epoch-diff --state open` and
   `gh issue list --label prod-readiness --state open`.
6. **Detect new divergences.** A fresh sync halt (`WithdrawalAmountMismatch`,
   chain_diverged) or a newly-surfaced epoch diff becomes a NEW backlog item
   (ranked by impact) in STATE.
7. **Reclaim stale state.** `heavyop-lock.sh status` — if held by a dead pid /
   past TTL, it self-reclaims on next acquire (no action). Clear any orphaned
   `.git/index.lock` if no git process is running. Prune abandoned worktrees:
   `git worktree prune`.
8. **Budget read.** Sum the rolling 24 h of the *Token spend* lines (each line is
   `YYYY-MM-DDThh:mmZ <tokens>`). Compare to `Control: daily_token_budget`.

---

## Phase 2 — SCHEDULE  (pick exactly one item to advance)

1. **Continue in-flight work first.** If an item is mid-state-machine
   (REPRODUCING/ANALYZING/FIXING/VERIFYING/GAUNTLET) and not BLOCKED, advance
   **that** item — do not start a new one (don't strand replays/worktrees).
2. **Else select a fresh item** from *Backlog*, ranked by impact `[H]>[M]>[L]`,
   filtered by three constraints:
   - **Budget.** If rolling spend ≥ 80 % of `daily_token_budget`: only do cheap
     poll/analysis steps; do NOT spawn a fresh fan-out Workflow. At ≥ 100 %:
     only poll in-flight jobs, then go straight to RESCHEDULE at the ceiling.
   - **Heavy-op lock.** If a heavy op (replay/sync/build) is already running
     (`heavyop-lock.sh status` ≠ `free`), do not pick an item that needs another
     heavy op; pick a lock-free analysis item instead.
   - **RAM fit.** A from-genesis replay needs the RAM a live node holds. If
     `free_ram_gb` is too low for the replay and a live node is running, either
     pick a lock-free analysis item, OR — only if the replay item **strictly
     dominates** the running node's frontier work — SIGTERM that node (clean
     snapshot) to free RAM and record the decision.
3. **Anti-thrash.** Never SIGTERM a node that is actively advancing a gate
   frontier (sync climbing, soak accruing clean minutes) for a lower- or
   equal-impact replay. If a kill would regress a green-ish gate, defer the
   replay to the node's next natural checkpoint and pick another item. Record
   the dominance decision + frontier cost in STATE.
4. Set the chosen item as *In-progress* with its current state.

---

## Phase 3 — DRIVE  (run the phase for the item's STATE)

Branch on the item's state-machine state:

### REPRODUCING / VERIFYING  (out-of-band, loop-owned)
1. `bash scripts/prod-readiness/lib/heavyop-lock.sh acquire "<item-slug>"`
   (if it fails, the lock is busy → go back to SCHEDULE and pick a lock-free item).
2. Clone the relevant db (CoW, source untouched):
   `dest=$(bash scripts/prod-readiness/lib/clone-db.sh <src-db> <net>-<slug>)`.
   Source dbs: preprod `db-preprod-sync`, mainnet `db-mainnet`
   (`db-mainnet-pre-alonzo` for the d-window). Confirm with `ls` first.
3. Choose instrumentation by item class and `export` it before launch:
   - ledger item → `export DUGITE_EPOCH_STATE_DUMP=<dump-dir>`
   - phase-2 item → `export DUGITE_PHASE2_DUMP_DIR=<dump-dir>`
   - reward-account item → also `export DUGITE_REWARD_DBG=1`
4. Launch the background replay (caffeinate-wrapped):
   `job=$(bash scripts/prod-readiness/lib/launch-replay.sh <net>-<slug> "$dest" \
        --config config/<net>/config.json --network-magic <magic> --no-mithril)`
   then bind the lock to the job pid:
   `bash scripts/prod-readiness/lib/heavyop-lock.sh bind "$(cat scripts/prod-readiness/.jobs/<net>-<slug>.pid)"`.
   (Build the binary with the epoch-state-debug feature first if dumps are needed
   — see README. Adjust run flags to the item; `DUGITE_REPLAY_LIMIT` can bound it.)
5. Record the job under *Running jobs* in STATE with pid + log path + the
   epoch/account it is chasing. The item is now REPRODUCING (or VERIFYING).
   **Do not wait** — RESCHEDULE and poll it next wake.
6. When ASSESS later reports the job `done`: release the lock, then:
   - **REPRODUCING done** → run the muscle in `diagnose` mode (this is the
     dump-vs-Koios localization, as a `/workflows`-visible parallel fan-out):
     `Workflow({ scriptPath: "scripts/prod-readiness/muscle.workflow.js",
     args: { item, mode:"diagnose", net, reference, dumpPath:"<DUGITE_EPOCH_STATE_DUMP dir>" } })`.
     Record the localized divergence; move to ANALYZING.
   - **VERIFYING done** → move to GAUNTLET.

### DIAGNOSE / ANALYZING / FIXING / GAUNTLET  (in-turn, Workflow muscle)
ALL analytical work runs through the muscle so it is visible in `/workflows`
(mechanical steps — clone/launch/poll — stay as direct shell above). Spawn the
muscle with the right mode (one Workflow call):
```
Workflow({ scriptPath: "scripts/prod-readiness/muscle.workflow.js",
           args: { item: "<backlog line>", mode: "<diagnose|analyze|fix|gauntlet>",
                   net: "<preprod|mainnet>", reference: "<Koios/dump locator>",
                   dumpPath: "<dump dir, for diagnose>", tokenBudget: <remaining> } })
```
- **ANALYZING** (`mode:"analyze"`) → returns research (Haskell source + spec) +
  a structured root-cause. **Read the in-project refs FIRST** inside the muscle:
  `.claude/skills/haskell-ledger-cross-validation/references/era-rules/*.md`.
  Record the root-cause in STATE; advance to FIXING.
- **FIXING** (`mode:"fix"`) → patch in a worktree, classify the tier, run
  fmt+clippy+nextest. Record files + tier; then launch a VERIFYING replay
  (back to the out-of-band branch) to prove the divergence is gone.
- **GAUNTLET** (`mode:"gauntlet"`) → only after a VERIFYING replay reproduced the
  reference with the divergence gone. Runs the tier-appropriate checks below.
  **Commit + push only if the gauntlet passes.** Record pass/refute verbatim in
  the *Gauntlet ledger* so a REFUTED approach is never silently retried.

---

## Phase 4 — RECORD  (rewrite + commit STATE)

1. Rewrite `engine-state.md`: update the item's state/attempts, Frontiers,
   Running jobs, DB clones on disk, Gauntlet ledger, In-progress.
2. Append a *Token spend* line: `printf -- '- %sZ %s\n' "$(date -u +%Y-%m-%dT%H:%M)" "<approx output tokens this wake>"`.
3. Update *Last node state* with the health sample.
4. Commit it (audit trail; recoverable if a later step crashes):
   `git add scripts/prod-readiness/engine-state.md && git commit -m "engine: wake — <one-line summary>"`.
   (Use `--no-verify` only if the pre-commit hook is unrelated; otherwise let it run.)
5. GC disk if free space is tight:
   `bash scripts/prod-readiness/lib/gc-disk.sh gc-clones <net>` and `gc-dumps <net>`.

---

## Phase 5 — RESCHEDULE

Choose the next wake delay from what you're waiting on, honoring
`cadence_floor_secs`/`cadence_ceiling_secs` and the budget back-off:
- **~270 s** (cache-warm) while actively polling a replay you just launched.
- **1200–1800 s** when a multi-hour from-genesis sync/replay is the gate, or when
  budget is ≥ 80 % spent.
- **floor** when a fitting analysis item is queued and budget allows.

Then either `ScheduleWakeup(delay, prompt: "Execute one wake of
scripts/prod-readiness/engine-runbook.md")` for an in-session loop, OR rely on the
standing `CronCreate` schedule for an unattended run.

**Finally, release the wake-lock** so the next fire can run:
`bash scripts/prod-readiness/lib/wake-lock.sh release`. Then **stop** — this wake
is complete. (If you STOPPED early at the halt sentinel or a `busy` wake-lock, do
the appropriate thing: on halt, release nothing extra and don't reschedule; on
`busy`, you never acquired it, so release nothing.)

---

## Verification gauntlet (the autonomy gate — copy of the spec)

Every fix must clear its tier before any commit. Record the outcome (pass AND
every refutation) in the *Gauntlet ledger*.

### Tier A — reward / snapshot / era-transition / governance / fee math
All four, in order; any failure → reject, return item to backlog with the reason:
1. **Oracle Haskell-source exact match** — changed logic quotes canonical
   `IntersectMBO/cardano-ledger` source (via `cardano-haskell-oracle`) and matches it.
2. **Spec citation** — the relevant ledger-spec rule is quoted and consistent.
3. **Byte-exact replay reproduces reference** — a real replay over the window
   reproduces Koios / the cardano-node dump, divergence gone. Tests-green is NOT
   sufficient.
4. **Adversarial refutation panel** — `refuter_N` skeptics (default 3), distinct
   lenses; majority-refute → rejected.

### Tier A′ — phase-2 / ScriptContext schema fixes
1. Oracle Haskell-source match (Plutus `ScriptContext`/`TxInfo` builder).
2. **Field-diff vs a Haskell ScriptContext dump** (not just `is_valid` parity).
3. `phase2_repro` reproduces byte-exact ExBudget + `is_valid` across bucket reps
   (`cargo run -p dugite-uplc --example phase2_repro -- <dump.json>`).
4. Refutation panel (lenses: schema-nesting, era-fallback, builtin-cost, data-encoding).

### Tier B — other non-ledger (perf, network, CLI-format, decoder, tests, docs)
`cargo fmt --all -- --check` + `cargo clippy --all-targets -- -D warnings` +
`cargo nextest run --workspace` all green + one focused verifier + the relevant
devnet/soak harness shows no regression.

---

## Dimension playbooks (which existing asset drives each gate)

| Gate | Playbook |
|---|---|
| Ledger byte-exactness | `haskell-ledger-cross-validation` skill + clone-db replay + `lib/koios.sh` (REST, not MCP) + `DUGITE_EPOCH_STATE_DUMP` |
| Live sync to tip | `scripts/dev/{sync-to-tip,stall,passive-sync}-watch.sh` + `soak-sample` + `health-sample.sh` |
| Phase-2 / UPLC | `phase2_repro` example + `DUGITE_PHASE2_DUMP_DIR` + ScriptContext field-diff vs Haskell |
| Perf & robustness | `devnet-validate` skill + `prof-run`/`soak-sample` + security tooling |

## Autonomy invariants (restated)

Permission posture non-interactive (run `bootstrap.sh` once); no `AskUserQuestion`;
HTTPS+`gh` push; no PR-merge wait (`gh pr merge --auto --squash` if integrating to
main); Koios-first reference data; disk GC; stale lock/worktree breakers;
`caffeinate` wrap; idempotent crash recovery (STATE is the only memory). The only
human touchpoint is the halt sentinel.
