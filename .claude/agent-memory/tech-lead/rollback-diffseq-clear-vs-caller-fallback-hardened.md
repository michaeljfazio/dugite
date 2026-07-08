---
name: rollback-diffseq-clear-vs-caller-fallback-hardened
description: Live-tip 1-block fork rollback stall — DEFECT-A's diff_seq.clear() is the real root cause; the node-level snapshot-reload fallback is already hardened (refutes the naive "reloads latest snapshot regardless of target" theory)
metadata:
  type: project
---

## Context (2026-07-08 investigation)

A hypothetical/reported incident: live preprod BP soak hit a 1-block fork
switch at the tip; `rollback_via_seq` bailed with "DiffSeq too short to
cover rollback... have=0" (the #806 DEFECT-A guard,
`crates/dugite-ledger/src/state/mod.rs:2145-2152`), then later
`apply_fork_switch_plan` logged `BlockDoesNotConnect` and the node stalled
at the orphaned tip requiring manual snapshot-swap recovery.

## Finding 1 — the caller's slow-path fallback is NOT naive (refutes a plausible theory)

The obvious hypothesis ("the fallback just reloads the newest on-disk
snapshot regardless of whether it's older than the rollback target") is
**refuted by current source**. `find_best_snapshot_for_rollback`
(`crates/dugite-node/src/node/epoch.rs:572-674`) explicitly requires
`snap_slot <= rollback_slot` for every candidate (both epoch-numbered and
`ledger-snapshot.bin`), checks canonicality via `is_snapshot_canonical`,
and returns `None` (not a too-new snapshot) when no candidate qualifies.
The caller (`handle_rollback_inner`,
`crates/dugite-node/src/node/sync.rs:451-722`) restores the matching LSM
UTxO snapshot generation, replays `ApplyOnly` forward to the target, and
has an explicit defense-in-depth guard (`final_slot < rollback_slot ⇒
return false`, sync.rs:644-660, comment dated 2026-05-29) — so a
truncated/failed replay cannot silently report success. When no snapshot
qualifies at all, it explicitly `return false` (sync.rs:704-721) rather
than falling back to a wrong-tip snapshot. `apply_fork_switch_plan`
(`crates/dugite-node/src/node/mod.rs:6202-6212`, "Fix C / Bug B") checks
this return value and aborts the fork replay (does NOT `clear_volatile`)
when it's `false` — so under current code, `BlockDoesNotConnect` during
fork replay should be unreachable via this specific path; the reachable
failure mode instead is a **silent `Aborted` + permanent park at the
orphaned tip** (no automatic retry evident — `apply_fetched_block`/LoE
reprocess callers just discard the `Aborted` result).

Do not re-propose "harden the snapshot-reload fallback to check the
target slot" as a fix — it's already implemented. If a future incident
shows literal `BlockDoesNotConnect` text (not just a stall), suspect
either an older binary predating this hardening, or a NEW bug distinct
from DEFECT-A.

## Finding 2 — real root cause: vestigial diff_seq.clear() defeats an already-correct k-bounded window

`crates/dugite-ledger/src/state/apply.rs:681-686` (Byron) and
`:1792-1797` (Shelley+) already call
`self.utxo.diff_seq.push_bounded(..., self.security_param as usize)` on
**every** block apply (both `ApplyOnly` and `ValidateAll`, unconditional
on validation mode) — `diff_seq` is *already* a proper k-bounded rolling
window by design (comment at apply.rs:1775-1791 explicitly says so,
citing the ~27GB from-genesis OOM that `push_bounded` fixed by replacing
an unbounded `push`).

Despite that, `crates/dugite-node/src/node/epoch.rs:369`
(`save_ledger_snapshot`) and `:511` (`try_snapshot_async`) *also*
unconditionally call `ls.utxo.diff_seq.clear()` on every snapshot save —
wiping the k-bounded window to 0 regardless of how many blocks are in it.
The comment justifies this as "free memory since diffs are
`#[serde(skip)]`" (confirmed at
`crates/dugite-ledger/src/state/snapshot_format.rs:192-194`) — but since
`push_bounded` already caps the live in-memory size at k entries
(preprod/mainnet k=2160, preview k=432; worst-case ballpark ~tens of MB,
not the unbounded-growth scenario the OOM postmortem was about), there is
**no real memory benefit** to the periodic full clear — it looks like a
vestige of the pre-`push_bounded` blunt OOM mitigation that was never
removed once the real (bounded) fix landed. `seq` (`LedgerSeq`) is NOT
cleared by snapshot save — only `diff_seq` is — which is exactly the
asymmetry DEFECT-A's guard defends against.

**Recommended fix**: delete both `ls.utxo.diff_seq.clear()` calls
(epoch.rs:369, :511). This closes the gap at the source instead of only
defending against it: with `diff_seq` never force-emptied, any rollback
within the k-block volatile window can *always* be satisfied by the O(n)
fast path (`rollback_via_seq`), regardless of how recently a snapshot was
saved — eliminating the narrow post-snapshot window where a live-tip fork
switch is forced into the slow path (which itself can legitimately return
`false`/stall on a fresh node before a second snapshot generation exists).
`flush_up_to(gc_slot)` (sync.rs:1780, called on immutable-flush) remains
the correct/only trim mechanism beyond `push_bounded`'s own eviction.

## Finding 3 — this path is genuinely under-tested (confirms the task's suspicion)

`rollback_via_seq_detects_diff_seq_desync_and_bails_out`
(`crates/dugite-ledger/src/state/tests.rs:17430+`) only exercises the
low-level ledger guard (manually simulating `diff_seq.clear()`, asserting
`None`) — it never exercises the node-level fallback
(`find_best_snapshot_for_rollback` / `handle_rollback_inner` /
`apply_fork_switch_plan`). `grep` across `crates/dugite-node/tests/`
found zero references to `handle_rollback_inner`,
`find_best_snapshot_for_rollback`, or an end-to-end
snapshot-save-then-fork-switch sequence. Missing regression test: apply N
blocks (N < k) on a `Node` test harness with a real ChainDB, call
`save_ledger_snapshot()` (or `try_snapshot_async` + wait for the worker),
then feed a 1-block `TriggeredFork` and assert the ledger tip lands
exactly on the intersection and the sibling block applies cleanly
afterward (no `BlockDoesNotConnect`, no `Aborted`).

See also [[issues-805-806-807-813-batch-fix]] for the original #806
DEFECT-A guard this builds on.
