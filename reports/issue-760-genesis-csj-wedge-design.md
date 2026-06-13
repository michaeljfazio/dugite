# #760 — Genesis CSJ-far-ahead wedge (A) + SIGTERM shutdown robustness (B)

Verified by a 4-agent investigation workflow (root-cause + Haskell cross-check).
All file:line claims re-verified against the working tree.

## Part B — bounded SIGTERM watchdog — DONE (commit 7bae4bac27)

Root cause: cooperative shutdown routes through a watch channel; the only
process-exit is the post-loop drain, unreachable until the main run loop
breaks. The loop's shutdown arm was LAST in a non-`biased` select, starved by
the perpetually-ready fetched-block apply arm while the wedged tasks held the
ledger/chain_db locks. SIGTERM-to-exit was unbounded (observed 1h42m) → forced
SIGKILL → ImmutableDB/LSM corruption risk.

Fix (process-lifecycle only; praos byte-identical): `biased;` + shutdown arm
first; an independent watchdog that force-exits within
`DUGITE_SHUTDOWN_DEADLINE_SECS` (default 90s) if a `loop_broken` AtomicBool is
still false (wedged, never broke), while leaving a healthy slow drain (its own
30s/120s bounded force-exits) alone; a second signal forces immediate exit; the
watchdog never touches ledger_state/chain_db (relies on the last periodic
atomic snapshot), so it is safe even when the wedge holds those locks.

## Part A — CSJ-far-ahead wedge — DESIGN (awaits mainnet-scale validation)

### Root cause: a circular dependency that only primes on cold restart
On a cold genesis restart from a mid-chain snapshot:
- ledger-tip advance requires the LoE tip to advance
  (`chain_sel_queue.rs:419-428` trimToLoE caps adoption at `LoE_tip + k`,
  `loe_trim.rs:141-247`);
- the LoE tip (Syncing) = volatile window ++ `shared_candidate_prefix(ALL peer
  frags)` (`gsm.rs:900-980`, `genesis_governor.rs:113-154`) — the longest
  common prefix, gated by the laggiest of the 39 jumpers (they advance only on
  jump acceptance via `replace_fragment`, in `jump_size`=4320 steps);
- the dynamo's candidate fragment can't advance past `ledger_tip +
  stability_window` because `forecast_park_or_disconnect` (`sync.rs:~5247`)
  runs and parks the header BEFORE it is pushed to `pending_headers`
  (`sync.rs:~5372`) and before `csj.update_jump_info`/`on_roll_forward`
  (`sync.rs:~5407`);
- the forecast horizon = `ledger_tip + 1 + stability_window` — only moves when
  the ledger advances.

So: ledger needs LoE → LoE needs the jumpers' shared prefix → that needs the
dynamo fragment → that needs the forecast horizon → that needs the ledger. On a
warm (continuously-running) node these advance in lockstep as bodies stream;
on cold restart the snapshot frontier sits far below peers' real tips and the
cycle never primes. The watchdog rotation (`connection_lifecycle.rs:2431-2479`
→ `rotate_dynamo`) cannot break it: every new dynamo re-intersects at the
frontier (`reintersect_promoted_peer`, the #735 fix, `sync.rs:4348-4415`) and
re-hits the same frozen forecast horizon → ~1 blk/min → collapse. Praos avoids
it entirely (LoE Disabled, no CSJ).

### Fix design (genesis-gated; praos byte-identical)
A1 (load-bearing): in `chainsync_client_task` reorder so a RollForward header
is recorded into the candidate fragment + `pending_headers` +
`csj.update_jump_info`/`on_roll_forward` FIRST, then apply
`forecast_park_or_disconnect` as **pipeline backpressure** (do not request the
NEXT header until in range) rather than an **admission gate** for the current
one — gated on `genesis_bulk_sync`, bounded by `PENDING_HEADERS_PAUSE`
(10_000). This lets the dynamo fragment (→ LoE shared prefix → jumpers' targets)
run ahead of the ledger, so BlockFetch always has contiguous gap headers from
the frontier, the ledger advances k blocks, the horizon moves, and the cycle
self-primes. Mirrors Haskell, where ChainSync blocks at the forecast horizon
only for ISSUING MsgRequestNext, while `theirFrag`/`jTheirFragment` already
hold the streamed headers (dugite conflates the two because the fragment is
built at the same code point that parks).

A2 (only if A1 doesn't fully self-prime): make the genesis LoE tolerant of
in-flight headers so the LoE tip is not pinned by the slowest jumper.

### Why this is NOT yet implemented/merged
- It is a consensus-critical chainsync-hot-path change (MED risk): a mistake
  could break praos sync (gate error), grow `pending_headers` unbounded (OOM),
  or change selection.
- Validation requires reproducing the wedge, which is **mainnet-scale and
  timing-dependent** (many peers + CSJ jumping far ahead from a cold frontier);
  it will not reliably reproduce on preprod. A negative preprod result would be
  inconclusive.
- Plan of record: implement A1 genesis-gated, keep CSJ disabled on the mainnet
  config until a genesis-mode cold-restart mainnet sync demonstrably advances
  past `snapshot_tip + k` to the live tip. Part B (shipped) is the safety net
  that makes such validation runs safe to interrupt.

Workaround in the meantime: `--consensus-mode praos` bulk-syncs reliably
(~47k slots/min); ledger byte-exactness is identical regardless of mode.
