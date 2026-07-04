---
name: issue-767-slow-demotion-cascade
description: #767 residual Slow-demotion cascade — peer_failed for Slow demotes Hot→Cold without connection teardown, causing mass reconnect storm on apply pause
metadata:
  type: project
---

## Root cause of the self-sustaining cascade

A brief apply pause (>60s per-range) triggers a cascade:

1. `FETCH_RANGE_TIMEOUT = 60s` (connection_lifecycle.rs:54) fires → blockfetch worker sends `PeerFailureKind::Slow` to `peer_failure_tx` (cap=64, try_send) and exits.
2. Main loop processes `peer_failure_rx.recv()` at mod.rs:5167 → calls `pm.peer_failed(&failed_addr)` for EVERY slow peer.
3. `peer_failed()` (networking.rs:663) calls `record_failure()` (backoff) + `demote_to_cold()` — the peer moves to Cold IN THE PEER MANAGER even though the TCP connection+mux is still alive (the chainsync task is still running).
4. Because the peer is now Cold in PeerManager but the TCP mux is still running, `cleanup_dead_connections()` won't clean it up either (mux is alive). The peer sits in an inconsistent state.
5. For Slow failures, NO connection teardown happens (only `ProtocolFault` calls `lifecycle.demote_to_cold()`). The chainsync task keeps running, re-intersects to dugite's tip, and sends `MsgRollBackward rollback_point=slot:<tip>` — which is logged as the "ChainSync rollback" flood.
6. Governor tick (every 2s) sees: `hot_count` dropped (peers are Cold in PeerManager but TCP still alive) → fires `PromoteToHot` actions → `promote_to_hot()` fails with `NotConnected` or succeeds on an already-running chainsync → chainsync re-intersects → rollback-to-tip → Slow failure again.
7. No anti-cascade gate: the governor's `demote_cooldown` (300s) only applies to `aboveTargetOther` demotions (governor-initiated), NOT to `peer_failed()` paths. A Slow failure bypasses `record_demote()` entirely.

## Key invariants violated

- `peer_failed()` calls `demote_to_cold()` which puts peer in Cold state, but does NOT cancel the hot protocol tasks (chainsync/blockfetch are still running). The peer is Cold in the manager but still has live tasks on the connection.
- For Slow failures, the blockfetch task exits (that task's `return` drops the Guard), but the chainsync task is still alive. The governor now tries to re-promote (PromoteToHot) a peer whose chainsync is already running.
- `recently_demoted`/`record_demote()` is only called by the governor's `aboveTargetOther` path. `peer_failed()` never calls `record_demote()`, so there is NO cooldown on re-promotion after a Slow failure.

## The "self-sustaining" answer

YES: The cascade is self-sustaining once it starts:
- apply pause >60s → Slow → peer_failed → demote_to_cold (manager only, no TCP teardown) → governor re-promotes → chainsync re-intersects → rollback-to-tip → more header-prune churn → apply stays stalled → next batch of 60s timeouts fires.
- The cycle period is approximately 60s (FETCH_RANGE_TIMEOUT) with 48 peers all cycling.

## Fix direction

Option A: `peer_failed()` for Slow should call `record_demote()` (or an equivalent backoff gate at the governor level) to prevent immediate re-promotion.
Option B: Slow failures should also tear down the hot protocols (matching Haskell behavior: timeouts DO kill the bearer in Haskell), so the connection cleanly re-enters Cold→Warm→Hot with proper backoff.
Option C: Reduce FETCH_RANGE_TIMEOUT from 60s to a value below the typical apply pause (~5-7s was observed) — but this is fragile.

**Why:** Haskell kills the bearer on timeout (throwing an exception that closes the multiplexer); dugite intentionally deviates (comment at connection_lifecycle.rs:202-206, tracked as follow-up). That deviation removes the natural backoff that Haskell gets from TCP reconnect latency, leaving dugite with zero cooldown between Slow failures.
